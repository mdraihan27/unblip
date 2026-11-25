pub mod clipboard;
pub mod shortcut;
pub mod storage;
pub mod window;

use clipboard::{
    start_clipboard_watcher, write_image_data_url_to_clipboard, write_text_to_system_clipboard,
};
use shortcut::{register_shortcut, sync_autostart_desktop_file};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use storage::{
    AppSettings, ClipboardItem, ContentType, FrequentItem, PersistentData, SharedStorage,
    StorageState,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use window::{
    begin_window_drag, commit_text_to_active_input_box, configure_window_focusable,
    has_active_text_cursor, hide_popup, is_pointer_inside_unblip_window,
    is_window_dragging_recently, mark_window_drag_ended, record_window_moved, set_maximized_mode,
    set_pointer_inside_webview, show_popup_near_cursor, start_ibus_cursor_tracker,
    step_window_drag, toggle_popup,
};

pub fn ipc_socket_path() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/unblip-{}.sock", uid))
}

/// Sends a command to an already-running Unblip instance over the Unix socket.
/// Returns `true` if a running instance handled the command.
pub fn try_send_ipc_command(cmd: &str) -> bool {
    let sock_path = ipc_socket_path();
    if let Ok(mut stream) = UnixStream::connect(&sock_path) {
        if stream.write_all(cmd.as_bytes()).is_ok() {
            return true;
        }
    }
    false
}

fn start_ipc_server(app: AppHandle, storage: SharedStorage) {
    let sock_path = ipc_socket_path();
    let _ = fs::remove_file(&sock_path);

    if let Ok(listener) = UnixListener::bind(&sock_path) {
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut s = stream;
                let mut buf = String::new();
                if s.read_to_string(&mut buf).is_ok() {
                    let msg = buf.trim();
                    let near_cursor = storage
                        .data
                        .lock()
                        .map(|d| d.settings.show_near_cursor)
                        .unwrap_or(true);
                    match msg {
                        "TOGGLE" => toggle_popup(&app, near_cursor),
                        "SHOW" => show_popup_near_cursor(&app, near_cursor),
                        "HIDE" => hide_popup(&app),
                        _ => {}
                    }
                }
            }
        });
    }
}

#[tauri::command]
fn get_app_state(storage: State<'_, SharedStorage>) -> Result<PersistentData, String> {
    let guard = storage.data.lock().map_err(|e| e.to_string())?;
    Ok(guard.clone())
}

#[tauri::command]
fn set_history_enabled(
    app: AppHandle,
    storage: State<'_, SharedStorage>,
    enabled: bool,
) -> Result<AppSettings, String> {
    let updated = {
        let mut guard = storage.data.lock().map_err(|e| e.to_string())?;
        guard.settings.history_enabled = enabled;
        if !enabled && guard.items.len() > 1 {
            guard.items.truncate(1);
        }
        storage.save_locked(&guard);
        guard.settings.clone()
    };
    let _ = app.emit("clipboard-updated", ());
    Ok(updated)
}

#[tauri::command]
fn select_clipboard_item(
    app: AppHandle,
    storage: State<'_, SharedStorage>,
    id: String,
    should_hide: Option<bool>,
) -> Result<Option<ClipboardItem>, String> {
    let is_explicit_menu_copy = should_hide == Some(false);

    // If the user clicked a clipboard card directly (not the '...' -> 'Copy' menu action)
    // and no text input box is currently selected, do nothing and do NOT close the window!
    if !is_explicit_menu_copy && !has_active_text_cursor() {
        return Ok(None);
    }

    let updated = storage.record_item_clicked(&id);
    let mut text_to_paste: Option<String> = None;

    if let Some(ref item) = updated {
        if item.content_type == ContentType::Image {
            write_image_data_url_to_clipboard(&storage, &item.content)?;
        } else {
            write_text_to_system_clipboard(&storage, &item.content)?;
            text_to_paste = Some(item.content.clone());
        }
    }

    let (auto_paste, close_on_input) = storage
        .data
        .lock()
        .map(|d| (d.settings.auto_paste, d.settings.close_on_input))
        .unwrap_or((true, false));

    // Ensure window does not hold focus so the target input box receives the commit
    configure_window_focusable(&app, false);

    if auto_paste && has_active_text_cursor() {
        if let Some(ref txt) = text_to_paste {
            let _ = commit_text_to_active_input_box(txt);
        }
    }

    if !is_explicit_menu_copy && close_on_input && has_active_text_cursor() {
        hide_popup(&app);
    }

    let _ = app.emit("clipboard-updated", ());
    Ok(updated)
}

#[tauri::command]
fn select_symbol_or_emoji(
    app: AppHandle,
    storage: State<'_, SharedStorage>,
    symbol: String,
    name: String,
    kind: String,
    _should_hide: Option<bool>,
) -> Result<Vec<FrequentItem>, String> {
    // If no text input box is selected, do nothing and keep the window open
    if !has_active_text_cursor() {
        let current_freq = storage
            .data
            .lock()
            .map(|g| g.frequent_items.clone())
            .unwrap_or_default();
        return Ok(current_freq);
    }

    write_text_to_system_clipboard(&storage, &symbol)?;
    let frequent = storage.record_frequent_symbol(symbol.clone(), name, kind);

    let (auto_paste, close_on_input) = storage
        .data
        .lock()
        .map(|d| (d.settings.auto_paste, d.settings.close_on_input))
        .unwrap_or((true, false));

    configure_window_focusable(&app, false);

    if auto_paste {
        let _ = commit_text_to_active_input_box(&symbol);
    }

    if close_on_input {
        hide_popup(&app);
    }

    Ok(frequent)
}

#[tauri::command]
fn set_window_input_focus(app: AppHandle, focused: bool) {
    configure_window_focusable(&app, focused);
}

#[tauri::command]
fn notify_drag_started(app: AppHandle, offset_x: Option<i32>, offset_y: Option<i32>) {
    begin_window_drag(&app, offset_x.unwrap_or(184), offset_y.unwrap_or(15));
}

#[tauri::command]
fn drag_window_by(app: AppHandle, _dx: Option<i32>, _dy: Option<i32>) {
    step_window_drag(&app);
}

#[tauri::command]
fn notify_drag_ended() {
    mark_window_drag_ended();
}

#[tauri::command]
fn notify_pointer_inside(inside: bool) {
    set_pointer_inside_webview(inside);
}

#[tauri::command]
fn toggle_pin_item(
    app: AppHandle,
    storage: State<'_, SharedStorage>,
    id: String,
) -> Result<Option<bool>, String> {
    let pinned = storage.toggle_pin(&id);
    let _ = app.emit("clipboard-updated", ());
    Ok(pinned)
}

#[tauri::command]
fn delete_clipboard_item(
    app: AppHandle,
    storage: State<'_, SharedStorage>,
    id: String,
) -> Result<bool, String> {
    let deleted = storage.delete_item(&id);
    let _ = app.emit("clipboard-updated", ());
    Ok(deleted)
}

#[tauri::command]
fn clear_clipboard_history(
    app: AppHandle,
    storage: State<'_, SharedStorage>,
    include_pinned: bool,
) -> Result<usize, String> {
    let removed = storage.clear_all(include_pinned);
    let _ = app.emit("clipboard-updated", ());
    Ok(removed)
}

#[tauri::command]
fn clear_frequent_history(
    app: AppHandle,
    storage: State<'_, SharedStorage>,
) -> Result<usize, String> {
    let removed = storage.clear_frequent_items();
    let _ = app.emit("clipboard-updated", ());
    Ok(removed)
}

#[tauri::command]
fn add_manual_clipboard_item(
    app: AppHandle,
    storage: State<'_, SharedStorage>,
    text: String,
) -> Result<Option<ClipboardItem>, String> {
    write_text_to_system_clipboard(&storage, &text)?;
    let added = storage.add_text_item(text, "clipboard");
    let _ = app.emit("clipboard-updated", ());
    Ok(added)
}

#[tauri::command]
fn save_settings(
    app: AppHandle,
    storage: State<'_, SharedStorage>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    let prev_shortcut = storage
        .data
        .lock()
        .map(|d| d.settings.shortcut.clone())
        .unwrap_or_default();

    let saved = storage.update_settings(settings.clone());

    if saved.shortcut != prev_shortcut {
        let _ = register_shortcut(&app, &storage, &saved.shortcut);
    }

    let _ = sync_autostart_desktop_file(saved.autostart);
    let _ = app.emit("clipboard-updated", ());
    Ok(saved)
}

#[tauri::command]
fn hide_window(app: AppHandle) {
    hide_popup(&app);
}

#[tauri::command]
fn set_window_maximized_mode(app: AppHandle, maximized: bool) -> bool {
    set_maximized_mode(&app, maximized)
}

#[tauri::command]
fn show_window_at_cursor(app: AppHandle, storage: State<'_, SharedStorage>) {
    let near_cursor = storage
        .data
        .lock()
        .map(|d| d.settings.show_near_cursor)
        .unwrap_or(true);
    show_popup_near_cursor(&app, near_cursor);
}

fn setup_tray(app: &AppHandle, storage: SharedStorage) -> tauri::Result<()> {
    let toggle_i = MenuItem::with_id(app, "toggle", "Show / Hide Unblip", true, None::<&str>)?;
    let clear_i = MenuItem::with_id(app, "clear", "Clear Unpinned History", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "Quit Unblip", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle_i, &clear_i, &quit_i])?;

    let storage_menu = storage.clone();
    let storage_click = storage;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("Unblip — Windows 11 Clipboard & Emoji Manager")
        .on_menu_event(move |app_handle, event| match event.id.as_ref() {
            "toggle" => {
                let near_cursor = storage_menu
                    .data
                    .lock()
                    .map(|d| d.settings.show_near_cursor)
                    .unwrap_or(true);
                toggle_popup(app_handle, near_cursor);
            }
            "clear" => {
                storage_menu.clear_all(false);
                let _ = app_handle.emit("clipboard-updated", ());
            }
            "quit" => {
                app_handle.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(move |tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let near_cursor = storage_click
                    .data
                    .lock()
                    .map(|d| d.settings.show_near_cursor)
                    .unwrap_or(true);
                toggle_popup(tray.app_handle(), near_cursor);
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.set_icon(icon.clone());
        }
    }

    let _ = builder.build(app);
    Ok(())
}

pub fn run(start_hidden: bool) {
    let storage = StorageState::new(None);
    let storage_for_setup = storage.clone();
    let storage_for_events = storage.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(storage)
        .setup(move |app| {
            let handle = app.handle().clone();

            start_ibus_cursor_tracker(handle.clone(), storage_for_setup.clone());
            start_ipc_server(handle.clone(), storage_for_setup.clone());
            start_clipboard_watcher(handle.clone(), storage_for_setup.clone());

            let (shortcut_str, autostart, near_cursor) = {
                if let Ok(guard) = storage_for_setup.data.lock() {
                    (
                        guard.settings.shortcut.clone(),
                        guard.settings.autostart,
                        guard.settings.show_near_cursor,
                    )
                } else {
                    ("Super+V".to_string(), true, true)
                }
            };

            let _ = register_shortcut(&handle, &storage_for_setup, &shortcut_str);
            let _ = sync_autostart_desktop_file(autostart);
            let _ = setup_tray(&handle, storage_for_setup.clone());

            if start_hidden {
                hide_popup(&handle);
            } else {
                show_popup_near_cursor(&handle, near_cursor);
            }

            Ok(())
        })
        .on_window_event(move |window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                // Keep daemon running in background when X is clicked
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Moved(pos) => {
                record_window_moved(pos.x, pos.y);
            }
            WindowEvent::Focused(false) => {
                let hide_on_blur = storage_for_events
                    .data
                    .lock()
                    .map(|d| d.settings.hide_on_blur)
                    .unwrap_or(false);
                if hide_on_blur
                    && !is_window_dragging_recently()
                    && !is_pointer_inside_unblip_window(window.app_handle(), 18)
                {
                    hide_popup(window.app_handle());
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_app_state,
            set_history_enabled,
            select_clipboard_item,
            select_symbol_or_emoji,
            toggle_pin_item,
            delete_clipboard_item,
            clear_clipboard_history,
            clear_frequent_history,
            add_manual_clipboard_item,
            save_settings,
            hide_window,
            set_window_maximized_mode,
            set_window_input_focus,
            notify_drag_started,
            drag_window_by,
            notify_drag_ended,
            notify_pointer_inside,
            show_window_at_cursor,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Unblip Tauri application");
}

