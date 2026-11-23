use crate::storage::SharedStorage;
use crate::window::{ensure_safe_x11_error_handler, query_x11_pointer};
use arboard::{Clipboard, GetExtLinux, ImageData, LinuxClipboardKind, SetExtLinux};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use image::{ImageBuffer, Rgba};
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::ptr;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use x11::{xlib, xtest};

/// Persistent shared clipboard writer instance so Linux X11/Wayland selection ownership
/// is never dropped prematurely after `set_text` / `set_image`.
static WRITER_CLIPBOARD: OnceLock<Mutex<Option<Clipboard>>> = OnceLock::new();

fn with_writer_clipboard<F, R>(mut f: F) -> Result<R, String>
where
    F: FnMut(&mut Clipboard) -> Result<R, String>,
{
    let mutex = WRITER_CLIPBOARD.get_or_init(|| Mutex::new(Clipboard::new().ok()));
    let mut guard = mutex.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        *guard = Clipboard::new().ok();
    }
    if let Some(ref mut cb) = *guard {
        match f(cb) {
            Ok(val) => Ok(val),
            Err(_first_err) => {
                *guard = Clipboard::new().ok();
                if let Some(ref mut cb_retry) = *guard {
                    f(cb_retry)
                } else {
                    Err("Clipboard unavailable".to_string())
                }
            }
        }
    } else {
        Err("Failed to initialize system clipboard".to_string())
    }
}

pub fn start_clipboard_watcher(app: AppHandle, storage: SharedStorage) {
    thread::spawn(move || {
        ensure_safe_x11_error_handler();
        let mut clipboard_opt = Clipboard::new().ok();

        if let Some(ref mut cb) = clipboard_opt {
            if let Ok(initial_text) = cb.get_text() {
                if let Ok(mut last) = storage.last_clipboard_text.lock() {
                    *last = initial_text;
                }
            } else if let Ok(initial_img) = cb.get_image() {
                let h = hash_image(&initial_img);
                if let Ok(mut last_h) = storage.last_image_hash.lock() {
                    *last_h = h;
                }
            }
            if let Ok(initial_sel) = cb
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text()
            {
                if let Ok(mut last_sel) = storage.last_selection_text.lock() {
                    *last_sel = initial_sel;
                }
            }
        }

        let mut pending_selection: Option<(String, Instant)> = None;

        loop {
            thread::sleep(Duration::from_millis(120));

            if clipboard_opt.is_none() {
                clipboard_opt = Clipboard::new().ok();
            }
            let Some(ref mut cb) = clipboard_opt else {
                continue;
            };

            let (history_enabled, auto_copy_selection, sync_selection) = {
                if let Ok(guard) = storage.data.lock() {
                    (
                        guard.settings.history_enabled,
                        guard.settings.auto_copy_selection,
                        guard.settings.sync_selection_to_clipboard,
                    )
                } else {
                    (true, false, false)
                }
            };

            if !history_enabled {
                continue;
            }

            // 1. Check regular CLIPBOARD text
            if let Ok(text) = cb.get_text() {
                let mut should_add = false;
                if let Ok(mut last) = storage.last_clipboard_text.lock() {
                    if *last != text {
                        *last = text.clone();
                        should_add = true;
                    }
                }

                if should_add {
                    if let Ok(mut last_hash) = storage.last_image_hash.lock() {
                        *last_hash = 0;
                    }

                    let ignored = if let Ok(mut ign) = storage.ignore_next_clipboard.lock() {
                        if *ign {
                            *ign = false;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    if !ignored && !text.trim().is_empty() {
                        if storage.add_text_item(text, "clipboard").is_some() {
                            let _ = app.emit("clipboard-updated", ());
                        }
                    }
                }
            } else if let Ok(img) = cb.get_image() {
                // 2. Check regular CLIPBOARD image
                let hash = hash_image(&img);
                let mut is_new_img = false;
                if let Ok(mut last_hash) = storage.last_image_hash.lock() {
                    if *last_hash != hash && hash != 0 {
                        *last_hash = hash;
                        is_new_img = true;
                    }
                }

                if is_new_img {
                    if let Ok(mut last_text) = storage.last_clipboard_text.lock() {
                        last_text.clear();
                    }

                    let ignored = if let Ok(mut ign) = storage.ignore_next_clipboard.lock() {
                        if *ign {
                            *ign = false;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    if !ignored {
                        if let Some((data_url, w, h)) = encode_rgba_to_png_data_url(&img) {
                            if storage.add_image_item(data_url, w, h).is_some() {
                                let _ = app.emit("clipboard-updated", ());
                            }
                        }
                    }
                }
            }

            // 3. Check Linux PRIMARY selection ("Auto copy when text selected")
            if auto_copy_selection {
                if let Ok(sel_text) = cb
                    .get()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text()
                {
                    let trimmed = sel_text.trim();
                    if !trimmed.is_empty() {
                        let is_different = if let Ok(last_sel) = storage.last_selection_text.lock()
                        {
                            *last_sel != sel_text
                        } else {
                            false
                        };

                        if is_different {
                            match &pending_selection {
                                Some((prev_pending, _)) if prev_pending == &sel_text => {}
                                _ => {
                                    pending_selection = Some((sel_text.clone(), Instant::now()));
                                }
                            }
                        }
                    }
                }

                if let Some((ref candidate, since)) = pending_selection {
                    let button_pressed = query_x11_pointer()
                        .map(|p| p.button1_pressed)
                        .unwrap_or(false);

                    if !button_pressed && since.elapsed() >= Duration::from_millis(260) {
                        let finalized = candidate.clone();
                        pending_selection = None;

                        if let Ok(mut last_sel) = storage.last_selection_text.lock() {
                            *last_sel = finalized.clone();
                        }

                        if sync_selection {
                            if let Ok(mut last_cb) = storage.last_clipboard_text.lock() {
                                *last_cb = finalized.clone();
                            }
                            let _ = cb
                                .set()
                                .clipboard(LinuxClipboardKind::Clipboard)
                                .text(finalized.clone());
                        }

                        if storage.add_text_item(finalized, "selection").is_some() {
                            let _ = app.emit("clipboard-updated", ());
                        }
                    }
                }
            } else {
                pending_selection = None;
            }
        }
    });
}

fn hash_image(img: &ImageData) -> u64 {
    let mut hasher = DefaultHasher::new();
    img.width.hash(&mut hasher);
    img.height.hash(&mut hasher);
    let step = (img.bytes.len() / 256).max(1);
    for idx in (0..img.bytes.len()).step_by(step) {
        img.bytes[idx].hash(&mut hasher);
    }
    hasher.finish()
}

fn encode_rgba_to_png_data_url(img: &ImageData) -> Option<(String, u32, u32)> {
    let w = img.width as u32;
    let h = img.height as u32;
    if w == 0 || h == 0 {
        return None;
    }

    let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(w, h, img.bytes.to_vec())?;

    let mut png_bytes: Vec<u8> = Vec::new();
    buffer
        .write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .ok()?;

    let b64 = BASE64.encode(&png_bytes);
    Some((format!("data:image/png;base64,{}", b64), w, h))
}

pub fn write_text_to_system_clipboard(storage: &SharedStorage, text: &str) -> Result<(), String> {
    if let Ok(mut last) = storage.last_clipboard_text.lock() {
        *last = text.to_string();
    }
    if let Ok(mut last_sel) = storage.last_selection_text.lock() {
        *last_sel = text.to_string();
    }
    if let Ok(mut ign) = storage.ignore_next_clipboard.lock() {
        *ign = true;
    }

    let owned = text.to_string();
    with_writer_clipboard(|cb| cb.set_text(owned.clone()).map_err(|e| e.to_string()))
}

pub fn write_image_data_url_to_clipboard(
    storage: &SharedStorage,
    data_url: &str,
) -> Result<(), String> {
    let b64_part = data_url
        .strip_prefix("data:image/png;base64,")
        .ok_or_else(|| "Invalid PNG data URL".to_string())?;
    let png_bytes = BASE64.decode(b64_part).map_err(|e| e.to_string())?;
    let decoded = image::load_from_memory(&png_bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8();

    let (w, h) = decoded.dimensions();
    let img_data = ImageData {
        width: w as usize,
        height: h as usize,
        bytes: Cow::Owned(decoded.into_raw()),
    };

    let hash = hash_image(&img_data);
    if let Ok(mut last_hash) = storage.last_image_hash.lock() {
        *last_hash = hash;
    }
    if let Ok(mut ign) = storage.ignore_next_clipboard.lock() {
        *ign = true;
    }

    with_writer_clipboard(|cb| cb.set_image(img_data.clone()).map_err(|e| e.to_string()))
}

/// Simulates Ctrl+V keystroke via X11 XTest extension after focus returns to the user's previous window.
pub fn simulate_ctrl_v_paste() {
    thread::spawn(|| {
        ensure_safe_x11_error_handler();
        thread::sleep(Duration::from_millis(95));
        unsafe {
            let display = xlib::XOpenDisplay(ptr::null());
            if display.is_null() {
                return;
            }

            // XK_Control_L = 0xffe3, XK_v = 0x0076
            let ctrl_keycode = xlib::XKeysymToKeycode(display, 0xffe3);
            let v_keycode = xlib::XKeysymToKeycode(display, 0x0076);

            if ctrl_keycode != 0 && v_keycode != 0 {
                xtest::XTestFakeKeyEvent(display, ctrl_keycode as u32, 1, 0);
                xlib::XFlush(display);
                thread::sleep(Duration::from_millis(12));

                xtest::XTestFakeKeyEvent(display, v_keycode as u32, 1, 0);
                xlib::XFlush(display);
                thread::sleep(Duration::from_millis(12));

                xtest::XTestFakeKeyEvent(display, v_keycode as u32, 0, 0);
                xlib::XFlush(display);
                thread::sleep(Duration::from_millis(12));

                xtest::XTestFakeKeyEvent(display, ctrl_keycode as u32, 0, 0);
                xlib::XFlush(display);
            }

            xlib::XCloseDisplay(display);
        }
    });
}
