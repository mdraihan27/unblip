use crate::storage::SharedStorage;
use gtk::prelude::GtkWindowExt;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Position, Size};
use x11::xlib;

pub const POPUP_WIDTH: i32 = 368;
pub const POPUP_HEIGHT: i32 = 440;
pub const MAXIMIZED_WIDTH: i32 = 1020;
pub const MAXIMIZED_HEIGHT: i32 = 680;

static X11_ERR_HANDLER_INIT: Once = Once::new();
static IBUS_TRACKER_INIT: Once = Once::new();
static POPUP_VISIBLE: AtomicBool = AtomicBool::new(false);
static POPUP_MAXIMIZED: AtomicBool = AtomicBool::new(false);
static POINTER_INSIDE_WEBVIEW: AtomicBool = AtomicBool::new(false);
static DRAG_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn non_fatal_x11_error_handler(
    _display: *mut xlib::Display,
    _event: *mut xlib::XErrorEvent,
) -> i32 {
    0
}

pub fn ensure_safe_x11_error_handler() {
    X11_ERR_HANDLER_INIT.call_once(|| unsafe {
        xlib::XSetErrorHandler(Some(non_fatal_x11_error_handler));
    });
}

#[derive(Debug, Clone)]
pub struct CursorTrackingState {
    pub has_active_text_cursor: bool,
    pub text_cursor_rect: Option<(i32, i32, i32, i32)>,
    pub active_ic_path: String,
    pub ic_locations: HashMap<String, (i32, i32, i32, i32)>,
    pub last_appeared_spot: Option<(i32, i32)>,
    pub last_show_instant: Option<Instant>,
    pub last_hide_instant: Option<Instant>,
    pub last_toggle_instant: Option<Instant>,
    pub last_commit_instant: Option<Instant>,
    pub last_drag_instant: Option<Instant>,
    pub shown_with_active_cursor: bool,
    pub shown_ic_path: String,
    pub shown_cursor_rect: Option<(i32, i32, i32, i32)>,
    pub drag_offset: Option<(i32, i32)>,
}

impl Default for CursorTrackingState {
    fn default() -> Self {
        Self {
            has_active_text_cursor: false,
            text_cursor_rect: None,
            active_ic_path: String::new(),
            ic_locations: HashMap::new(),
            last_appeared_spot: None,
            last_show_instant: None,
            last_hide_instant: None,
            last_toggle_instant: None,
            last_commit_instant: None,
            last_drag_instant: None,
            shown_with_active_cursor: false,
            shown_ic_path: String::new(),
            shown_cursor_rect: None,
            drag_offset: None,
        }
    }
}

static TRACKER: OnceLock<Mutex<CursorTrackingState>> = OnceLock::new();
static IBUS_COMMITTER: OnceLock<Mutex<Option<ChildStdin>>> = OnceLock::new();

fn tracker_state() -> &'static Mutex<CursorTrackingState> {
    TRACKER.get_or_init(|| Mutex::new(CursorTrackingState::default()))
}

pub fn has_active_text_cursor() -> bool {
    tracker_state()
        .lock()
        .map(|st| st.has_active_text_cursor)
        .unwrap_or(false)
}

const IBUS_COMMITTER_PY: &str = r#"
import sys, json
import gi
gi.require_version('IBus', '1.0')
from gi.repository import IBus, GLib

IBus.init()
bus = IBus.Bus()
conn = bus.get_connection()
panel_ext = IBus.PanelService(
    connection=conn,
    object_path="/org/freedesktop/IBus/Panel/Extension/Emoji"
)
bus.request_name(
    "org.freedesktop.IBus.Panel.Extension.Emoji",
    IBus.BusNameFlag.REPLACE_EXISTING | IBus.BusNameFlag.ALLOW_REPLACEMENT
)

def commit_now(text):
    try:
        panel_ext.commit_text(IBus.Text.new_from_string(text))
        panel_ext.panel_extension(IBus.ExtensionEvent(name="emoji", is_enabled=False, is_extension=True))
    except Exception:
        pass
    return False

def on_stdin(channel, cond):
    line = channel.readline()
    if not line:
        loop.quit()
        return False
    try:
        data = json.loads(line)
        txt = data.get("text", "")
        if txt:
            panel_ext.panel_extension(IBus.ExtensionEvent(name="emoji", is_enabled=True, is_extension=True))
            GLib.timeout_add(25, commit_now, txt)
    except Exception:
        pass
    return True

io_chan = GLib.IOChannel.unix_new(sys.stdin.fileno())
GLib.io_add_watch(io_chan, GLib.IO_IN | GLib.IO_HUP, on_stdin)
loop = GLib.MainLoop()
loop.run()
"#;

fn ensure_ibus_committer() -> &'static Mutex<Option<ChildStdin>> {
    IBUS_COMMITTER.get_or_init(|| {
        let stdin_opt = Command::new("python3")
            .args(["-c", IBUS_COMMITTER_PY])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
            .and_then(|mut child| child.stdin.take());
        Mutex::new(stdin_opt)
    })
}

/// Directly commits text (emoji, kaomoji, symbol, or clipboard snippet) into the currently
/// focused input box via IBus PanelService without stealing focus or triggering Ubuntu's
/// XTest Remote Desktop permission dialog. Does nothing if no input box is currently active.
pub fn commit_text_to_active_input_box(text: &str) -> bool {
    if !has_active_text_cursor() {
        return false;
    }

    if let Ok(mut st) = tracker_state().lock() {
        st.last_commit_instant = Some(Instant::now());
    }

    let mutex = ensure_ibus_committer();
    if let Ok(mut guard) = mutex.lock() {
        if guard.is_none() {
            *guard = Command::new("python3")
                .args(["-c", IBUS_COMMITTER_PY])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
                .and_then(|mut child| child.stdin.take());
        }
        if let Some(ref mut stdin) = *guard {
            let payload = serde_json::json!({ "text": text }).to_string() + "\n";
            if stdin.write_all(payload.as_bytes()).is_ok() && stdin.flush().is_ok() {
                return true;
            } else {
                *guard = None;
            }
        }
    }
    false
}

/// Configures whether Unblip's GTK window accepts keyboard focus.
/// By default (`false`), clicking emojis/items inside Unblip does NOT steal keyboard focus
/// from the user's active input box. When the user clicks inside Unblip's search bar (`true`),
/// keyboard focus is temporarily enabled for typing.
pub fn configure_window_focusable(app: &AppHandle, focusable: bool) {
    if let Some(window) = app.get_webview_window("main") {
        if let Ok(gtk_win) = window.gtk_window() {
            gtk_win.set_accept_focus(focusable);
            gtk_win.set_focus_on_map(focusable);
        }
        if focusable {
            let _ = window.set_focus();
        }
    }
}

/// Updates whether the pointer is currently inside the Unblip webview (from DOM mouseenter/mouseleave/mousedown).
pub fn set_pointer_inside_webview(inside: bool) {
    POINTER_INSIDE_WEBVIEW.store(inside, Ordering::Relaxed);
}

/// Marks that the user touched the top drag bar or is actively dragging the window,
/// recording the pointer offset `(offset_x, offset_y)` inside the window.
pub fn mark_window_drag_active() {
    POINTER_INSIDE_WEBVIEW.store(true, Ordering::Relaxed);
    DRAG_IN_PROGRESS.store(true, Ordering::Relaxed);
    if let Ok(mut st) = tracker_state().lock() {
        st.last_drag_instant = Some(Instant::now());
    }
}

/// Starts a custom X11 pointer-followed window drag using the pointer's exact offset inside the top bar.
pub fn begin_window_drag(app: &AppHandle, offset_x: i32, offset_y: i32) {
    mark_window_drag_active();
    let mut final_offset = (offset_x.max(0), offset_y.max(0));

    // If X11 pointer and current window spot are known, cross-check offset
    if let Some(info) = query_x11_pointer() {
        let win_pos = tracker_state()
            .lock()
            .ok()
            .and_then(|st| st.last_appeared_spot)
            .or_else(|| {
                app.get_webview_window("main")
                    .and_then(|w| w.outer_position().ok())
                    .map(|p| (p.x, p.y))
            });
        if let Some((wx, wy)) = win_pos {
            let calc_ox = info.cursor_x - wx;
            let calc_oy = info.cursor_y - wy;
            let (ww, wh) = if POPUP_MAXIMIZED.load(Ordering::Relaxed) {
                (MAXIMIZED_WIDTH, MAXIMIZED_HEIGHT)
            } else {
                (POPUP_WIDTH, POPUP_HEIGHT)
            };
            if calc_ox >= 0 && calc_ox <= ww && calc_oy >= 0 && calc_oy <= wh {
                final_offset = (calc_ox, calc_oy);
            }
        }
    }

    if let Ok(mut st) = tracker_state().lock() {
        st.drag_offset = Some(final_offset);
        st.last_drag_instant = Some(Instant::now());
    }
}

/// Moves the window during an active drag so it follows the X11 global mouse pointer smoothly in all 4 directions.
pub fn step_window_drag(app: &AppHandle) {
    if !DRAG_IN_PROGRESS.load(Ordering::Relaxed) {
        return;
    }
    let Some(info) = query_x11_pointer() else {
        return;
    };
    if !info.button1_pressed {
        mark_window_drag_ended();
        return;
    }
    let (ox, oy) = tracker_state()
        .lock()
        .ok()
        .and_then(|st| st.drag_offset)
        .unwrap_or((POPUP_WIDTH / 2, 15));

    let new_x = (info.cursor_x - ox).clamp(0, (info.screen_width - 80).max(0));
    let new_y = (info.cursor_y - oy).clamp(0, (info.screen_height - 40).max(0));

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_position(Position::Physical(PhysicalPosition { x: new_x, y: new_y }));
        if !POPUP_MAXIMIZED.load(Ordering::Relaxed) {
            if let Ok(mut st) = tracker_state().lock() {
                st.last_appeared_spot = Some((new_x, new_y));
                st.last_drag_instant = Some(Instant::now());
            }
        }
    }
}

/// Marks that the user released the top drag bar after clicking or dragging it.
pub fn mark_window_drag_ended() {
    DRAG_IN_PROGRESS.store(false, Ordering::Relaxed);
    if let Ok(mut st) = tracker_state().lock() {
        st.drag_offset = None;
        st.last_drag_instant = Some(Instant::now());
    }
}

/// Returns true if a window drag is currently in progress or finished within the last 850ms.
pub fn is_window_dragging_recently() -> bool {
    if DRAG_IN_PROGRESS.load(Ordering::Relaxed) {
        return true;
    }
    tracker_state()
        .lock()
        .ok()
        .and_then(|st| st.last_drag_instant)
        .map(|t| t.elapsed() < Duration::from_millis(850))
        .unwrap_or(false)
}

/// Checks whether the pointer is inside the Unblip window (reported by WebView DOM, cross-checked with X11 geometry).
pub fn is_pointer_inside_unblip_window(app: &AppHandle, _pad: i32) -> bool {
    if DRAG_IN_PROGRESS.load(Ordering::Relaxed) {
        return true;
    }
    if !POINTER_INSIDE_WEBVIEW.load(Ordering::Relaxed) {
        return false;
    }

    // WebView thinks pointer is inside; cross-check with X11 geometry in case mouseleave was missed
    let eff_pad = 16;
    let mut geom_checked = false;
    if let (Some(info), Some(window)) = (query_x11_pointer(), app.get_webview_window("main")) {
        if let (Ok(pos), Ok(sz)) = (window.outer_position(), window.outer_size()) {
            geom_checked = true;
            let wx = pos.x;
            let wy = pos.y;
            let ww = sz.width as i32;
            let wh = sz.height as i32;
            if info.cursor_x >= wx - eff_pad
                && info.cursor_x <= wx + ww + eff_pad
                && info.cursor_y >= wy - eff_pad
                && info.cursor_y <= wy + wh + eff_pad
            {
                return true;
            }
        }
        if let Some(last_spot) = tracker_state()
            .lock()
            .ok()
            .and_then(|st| st.last_appeared_spot)
        {
            geom_checked = true;
            let (wx, wy) = last_spot;
            let (ww, wh) = if POPUP_MAXIMIZED.load(Ordering::Relaxed) {
                (MAXIMIZED_WIDTH, MAXIMIZED_HEIGHT)
            } else {
                (POPUP_WIDTH, POPUP_HEIGHT)
            };
            if info.cursor_x >= wx - eff_pad
                && info.cursor_x <= wx + ww + eff_pad
                && info.cursor_y >= wy - eff_pad
                && info.cursor_y <= wy + wh + eff_pad
            {
                return true;
            }
        }
    }

    !geom_checked
}

/// Updates the remembered window position when the user manually drags the popup window.
pub fn record_window_moved(x: i32, y: i32) {
    if POPUP_VISIBLE.load(Ordering::Relaxed) {
        if let Ok(mut st) = tracker_state().lock() {
            let settled = st
                .last_show_instant
                .map(|t| t.elapsed() > Duration::from_millis(120))
                .unwrap_or(true);
            if settled {
                st.last_drag_instant = Some(Instant::now());
                // Ignore bogus XWayland (x, 0..32) top-panel origin reports unless we are actively dragging near the top
                let valid_coords = x > 0 && (y > 32 || DRAG_IN_PROGRESS.load(Ordering::Relaxed));
                if !POPUP_MAXIMIZED.load(Ordering::Relaxed) && valid_coords {
                    st.last_appeared_spot = Some((x, y));
                }
            }
        }
    }
}

/// Switches the window between compact popup mode (`368x440`) and full-sized desktop sidebar mode (`1020x680`).
pub fn set_maximized_mode(app: &AppHandle, maximized: bool) -> bool {
    if let Some(window) = app.get_webview_window("main") {
        let x11_info = query_x11_pointer();
        let (screen_w, screen_h) = x11_info
            .map(|i| (i.screen_width, i.screen_height))
            .unwrap_or((1920, 1080));

        if maximized {
            // Save current compact spot before expanding
            if !POPUP_MAXIMIZED.load(Ordering::Relaxed) {
                if let Ok(pos) = window.outer_position() {
                    if pos.x >= 0 && pos.y >= 0 {
                        if let Ok(mut st) = tracker_state().lock() {
                            st.last_appeared_spot = Some((pos.x, pos.y));
                        }
                    }
                }
            }
            POPUP_MAXIMIZED.store(true, Ordering::Relaxed);

            let target_w = MAXIMIZED_WIDTH.min(screen_w - 48).max(720);
            let target_h = MAXIMIZED_HEIGHT.min(screen_h - 64).max(520);
            let target_x = ((screen_w - target_w) / 2).max(16);
            let target_y = ((screen_h - target_h) / 2).max(16);

            let _ = window.set_resizable(true);
            let _ = window.set_size(Size::Physical(PhysicalSize {
                width: target_w as u32,
                height: target_h as u32,
            }));
            let _ = window.set_position(Position::Physical(PhysicalPosition {
                x: target_x,
                y: target_y,
            }));
            let win_clone = window.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(30));
                let _ = win_clone.set_size(Size::Physical(PhysicalSize {
                    width: target_w as u32,
                    height: target_h as u32,
                }));
                let _ = win_clone.set_position(Position::Physical(PhysicalPosition {
                    x: target_x,
                    y: target_y,
                }));
            });
        } else {
            POPUP_MAXIMIZED.store(false, Ordering::Relaxed);
            let restore_pos = tracker_state()
                .lock()
                .ok()
                .and_then(|st| st.last_appeared_spot)
                .unwrap_or(((screen_w - POPUP_WIDTH) / 2, (screen_h - POPUP_HEIGHT) / 2));

            let _ = window.set_size(Size::Physical(PhysicalSize {
                width: POPUP_WIDTH as u32,
                height: POPUP_HEIGHT as u32,
            }));
            let _ = window.set_resizable(false);
            let _ = window.set_position(Position::Physical(PhysicalPosition {
                x: restore_pos.0,
                y: restore_pos.1,
            }));
            let win_clone = window.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(30));
                let _ = win_clone.set_size(Size::Physical(PhysicalSize {
                    width: POPUP_WIDTH as u32,
                    height: POPUP_HEIGHT as u32,
                }));
                let _ = win_clone.set_position(Position::Physical(PhysicalPosition {
                    x: restore_pos.0,
                    y: restore_pos.1,
                }));
            });
        }
    }
    POPUP_MAXIMIZED.load(Ordering::Relaxed)
}

/// Discovers the active IBus D-Bus socket address from `~/.config/ibus/bus/*`.
fn find_active_ibus_address() -> Option<String> {
    let config_dir = dirs::config_dir()?;
    let bus_dir = config_dir.join("ibus").join("bus");
    let entries = fs::read_dir(bus_dir).ok()?;

    let mut files: Vec<( std::time::SystemTime, PathBuf )> = entries
        .flatten()
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .collect();

    // Newest first
    files.sort_by(|a, b| b.0.cmp(&a.0));

    for (_, path) in files {
        if let Ok(content) = fs::read_to_string(&path) {
            let mut addr: Option<String> = None;
            let mut pid: Option<u32> = None;
            for line in content.lines() {
                if let Some(a) = line.strip_prefix("IBUS_ADDRESS=") {
                    addr = Some(a.trim().to_string());
                } else if let Some(p) = line.strip_prefix("IBUS_DAEMON_PID=") {
                    pid = p.trim().parse::<u32>().ok();
                }
            }
            if let (Some(a), Some(p)) = (addr, pid) {
                if PathBuf::from(format!("/proc/{}", p)).exists() {
                    return Some(a);
                }
            }
        }
    }
    None
}

/// Returns true if an outside-click/focus-change event should dismiss the popup right now.
fn should_dismiss_on_outside_event(
    app: &AppHandle,
    storage: &SharedStorage,
    min_show_ms: u64,
) -> bool {
    if !POPUP_VISIBLE.load(Ordering::Relaxed) {
        return false;
    }
    let hide_on_outside = storage
        .data
        .lock()
        .map(|g| g.settings.hide_on_blur)
        .unwrap_or(true);
    if !hide_on_outside {
        return false;
    }
    let timing_ok = tracker_state()
        .lock()
        .ok()
        .map(|st| {
            let show_ok = st
                .last_show_instant
                .map(|t| t.elapsed() >= Duration::from_millis(min_show_ms))
                .unwrap_or(true);
            let commit_ok = st
                .last_commit_instant
                .map(|t| t.elapsed() >= Duration::from_millis(550))
                .unwrap_or(true);
            show_ok && commit_ok
        })
        .unwrap_or(true);

    timing_ok && !is_window_dragging_recently() && !is_pointer_inside_unblip_window(app, 24)
}

const ATSPI_OUTSIDE_CLICK_PY: &str = r#"
import sys, time
import gi
gi.require_version('Atspi', '2.0')
from gi.repository import Atspi, Gio, GLib

# Ensure AT-SPI2 event delivery is enabled on the session accessibility bus
try:
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    bus.call_sync(
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.freedesktop.DBus.Properties",
        "Set",
        GLib.Variant("(ssv)", ("org.a11y.Status", "IsEnabled", GLib.Variant("b", True))),
        None,
        Gio.DBusCallFlags.NONE,
        1000,
        None,
    )
except Exception:
    pass

Atspi.init()
last_emit = 0.0

def on_event(event):
    global last_emit
    try:
        if event.type == "object:state-changed:focused" and event.detail1 != 1:
            return
        src = event.source
        if src is not None:
            app = src.get_application()
            if app is not None:
                name = (app.get_name() or "").lower()
                if "unblip" in name:
                    return
        now = time.monotonic()
        if now - last_emit < 0.08:
            return
        last_emit = now
        sys.stdout.write("OUTSIDE_EVENT\n")
        sys.stdout.flush()
    except Exception:
        pass

listener = Atspi.EventListener.new(on_event)
for ev in (
    "mouse:button:1p",
    "mouse:button:2p",
    "mouse:button:3p",
    "window:activate",
    "object:state-changed:focused",
):
    listener.register(ev)

Atspi.event_main()
"#;

/// Starts background listeners for:
/// 1. Real-time IBus text input box focus (`FocusIn`/`FocusOut`) and text cursor coordinates (`SetCursorLocation`),
/// 2. Outside-click dismissal when `hide_on_blur` ("Close when clicking outside") is enabled (X11 + IBus + AT-SPI2 Wayland).
pub fn start_ibus_cursor_tracker(app: AppHandle, storage: SharedStorage) {
    IBUS_TRACKER_INIT.call_once(move || {
        let app_ibus = app.clone();
        let storage_ibus = storage.clone();
        let app_atspi = app.clone();
        let storage_atspi = storage.clone();

        // Thread 1: X11 / XWayland outside mouse-button click detector, active-window watcher & smooth window drag follower
        let app_mouse = app;
        let storage_mouse = storage;
        thread::spawn(move || {
            let mut prev_button_down = false;
            let mut prev_active_win: Option<u64> = None;
            loop {
                let is_dragging = DRAG_IN_PROGRESS.load(Ordering::Relaxed);
                thread::sleep(Duration::from_millis(if is_dragging { 12 } else { 50 }));
                if !POPUP_VISIBLE.load(Ordering::Relaxed) {
                    prev_button_down = false;
                    prev_active_win = None;
                    continue;
                }

                if is_dragging {
                    step_window_drag(&app_mouse);
                    prev_button_down = DRAG_IN_PROGRESS.load(Ordering::Relaxed);
                    continue;
                }

                let hide_on_outside = storage_mouse
                    .data
                    .lock()
                    .map(|g| g.settings.hide_on_blur)
                    .unwrap_or(true);
                if !hide_on_outside {
                    prev_active_win = None;
                    continue;
                }

                let settled = tracker_state()
                    .lock()
                    .ok()
                    .and_then(|st| st.last_show_instant)
                    .map(|t| t.elapsed() >= Duration::from_millis(240))
                    .unwrap_or(true);
                if !settled {
                    continue;
                }

                if let Some(cur_win) = query_x11_active_window() {
                    if let Some(prev_win) = prev_active_win {
                        if cur_win != 0
                            && cur_win != prev_win
                            && should_dismiss_on_outside_event(&app_mouse, &storage_mouse, 450)
                        {
                            hide_popup(&app_mouse);
                            continue;
                        }
                    }
                    prev_active_win = Some(cur_win);
                }

                if let Some(info) = query_x11_pointer() {
                    if DRAG_IN_PROGRESS.load(Ordering::Relaxed) {
                        if !info.button1_pressed {
                            // Mouse button released after drag -> clear DRAG_IN_PROGRESS and start 650ms cooldown
                            DRAG_IN_PROGRESS.store(false, Ordering::Relaxed);
                            if let Ok(mut st) = tracker_state().lock() {
                                st.last_drag_instant = Some(Instant::now());
                            }
                        }
                        prev_button_down = info.button1_pressed;
                        continue;
                    }

                    if info.button1_pressed && !prev_button_down {
                        // Wait 30ms so any in-flight WebView DOM mousedown/notify_drag_started IPC arrives first
                        thread::sleep(Duration::from_millis(30));
                        if should_dismiss_on_outside_event(&app_mouse, &storage_mouse, 240) {
                            hide_popup(&app_mouse);
                        }
                    }
                    prev_button_down = info.button1_pressed;
                }
            }
        });

        // Thread 2: IBus D-Bus cursor tracker & Wayland outside-click detector
        thread::spawn(move || {
            loop {
                if let Some(ibus_addr) = find_active_ibus_address() {
                    // Ensure eavesdrop match rules on IBus Panel & InputContext
                    if let Ok(mut child) = Command::new("dbus-monitor")
                        .args([
                            "--address",
                            &ibus_addr,
                            "type='method_call',interface='org.freedesktop.IBus.InputContext'",
                            "type='method_call',interface='org.freedesktop.IBus.Panel'",
                        ])
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn()
                    {
                        if let Some(stdout) = child.stdout.take() {
                            let reader = BufReader::new(stdout);
                            let mut pending_member: Option<String> = None;
                            let mut pending_path: String = String::new();
                            let mut int_args: Vec<i32> = Vec::with_capacity(4);

                            for line in reader.lines().flatten() {
                                let trimmed = line.trim();

                                if trimmed.starts_with("method call ") {
                                    // Flush any previous member
                                    pending_member = None;
                                    int_args.clear();
                                    pending_path.clear();

                                    if let Some(p_idx) = trimmed.find("path=") {
                                        let rest = &trimmed[p_idx + 5..];
                                        if let Some(semi) = rest.find(';') {
                                            pending_path = rest[..semi].to_string();
                                        }
                                    }

                                    if let Some(m_idx) = trimmed.find("member=") {
                                        let member = trimmed[m_idx + 7..].trim();
                                        let is_visible = POPUP_VISIBLE.load(Ordering::Relaxed);

                                        match member {
                                            "SetCursorLocation" | "SetCursorLocationRelative" => {
                                                pending_member = Some(member.to_string());
                                            }
                                            "FocusIn" => {
                                                if !is_visible {
                                                    if let Ok(mut st) = tracker_state().lock() {
                                                        st.has_active_text_cursor = true;
                                                        if !pending_path.is_empty()
                                                            && pending_path.contains("InputContext_")
                                                        {
                                                            st.active_ic_path = pending_path.clone();
                                                            if let Some(&rect) =
                                                                st.ic_locations.get(&pending_path)
                                                            {
                                                                st.text_cursor_rect = Some(rect);
                                                            }
                                                        }
                                                    }
                                                } else if !pending_path.is_empty()
                                                    && pending_path.contains("InputContext_")
                                                {
                                                    // If popup is visible and user clicked into a NEW input box (when none was active or a different one)
                                                    let hide_on_outside = storage_ibus
                                                        .data
                                                        .lock()
                                                        .map(|g| g.settings.hide_on_blur)
                                                        .unwrap_or(true);
                                                    if hide_on_outside {
                                                        let should_dismiss = tracker_state()
                                                            .lock()
                                                            .ok()
                                                            .map(|st| {
                                                                let show_ok = st
                                                                    .last_show_instant
                                                                    .map(|t| {
                                                                        t.elapsed()
                                                                            >= Duration::from_millis(
                                                                                650,
                                                                            )
                                                                    })
                                                                    .unwrap_or(true);
                                                                let commit_ok = st
                                                                    .last_commit_instant
                                                                    .map(|t| {
                                                                        t.elapsed()
                                                                            >= Duration::from_millis(
                                                                                550,
                                                                            )
                                                                    })
                                                                    .unwrap_or(true);
                                                                let is_new_focus = !st
                                                                    .shown_with_active_cursor
                                                                    || (!st.shown_ic_path.is_empty()
                                                                        && st.shown_ic_path
                                                                            != pending_path);
                                                                show_ok && commit_ok && is_new_focus
                                                            })
                                                            .unwrap_or(false);
                                                        if should_dismiss
                                                            && !is_window_dragging_recently()
                                                            && !is_pointer_inside_unblip_window(
                                                                &app_ibus, 18,
                                                            )
                                                        {
                                                            hide_popup(&app_ibus);
                                                        }
                                                    }
                                                }
                                                pending_member = Some("FocusIn".to_string());
                                            }
                                            "FocusOut" | "DestroyContext" => {
                                                if !is_visible {
                                                    if let Ok(mut st) = tracker_state().lock() {
                                                        let recently_shown = st
                                                            .last_show_instant
                                                            .map(|t| {
                                                                t.elapsed()
                                                                    < Duration::from_millis(450)
                                                            })
                                                            .unwrap_or(false);
                                                        let recently_hidden = st
                                                            .last_hide_instant
                                                            .map(|t| {
                                                                t.elapsed()
                                                                    < Duration::from_millis(350)
                                                            })
                                                            .unwrap_or(false);
                                                        if !recently_shown && !recently_hidden {
                                                            st.has_active_text_cursor = false;
                                                        }
                                                    }
                                                } else {
                                                    // Popup is visible and the external input box lost focus (user clicked outside the input box!)
                                                    let hide_on_outside = storage_ibus
                                                        .data
                                                        .lock()
                                                        .map(|g| g.settings.hide_on_blur)
                                                        .unwrap_or(true);
                                                    if hide_on_outside {
                                                        let should_dismiss = tracker_state()
                                                            .lock()
                                                            .ok()
                                                            .map(|mut st| {
                                                                let show_ok = st
                                                                    .last_show_instant
                                                                    .map(|t| {
                                                                        t.elapsed()
                                                                            >= Duration::from_millis(
                                                                                650,
                                                                            )
                                                                    })
                                                                    .unwrap_or(true);
                                                                let commit_ok = st
                                                                    .last_commit_instant
                                                                    .map(|t| {
                                                                        t.elapsed()
                                                                            >= Duration::from_millis(
                                                                                550,
                                                                            )
                                                                    })
                                                                    .unwrap_or(true);
                                                                if show_ok && commit_ok {
                                                                    st.has_active_text_cursor =
                                                                        false;
                                                                    true
                                                                } else {
                                                                    false
                                                                }
                                                            })
                                                            .unwrap_or(false);
                                                        if should_dismiss
                                                            && !is_window_dragging_recently()
                                                            && !is_pointer_inside_unblip_window(
                                                                &app_ibus, 18,
                                                            )
                                                        {
                                                            hide_popup(&app_ibus);
                                                        }
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                } else if let Some(ref member) = pending_member {
                                    if member == "FocusIn" {
                                        // Panel.FocusIn passes `object path "/org/freedesktop/IBus/InputContext_N"`
                                        if let Some(q1) = trimmed.find('"') {
                                            if let Some(q2) = trimmed[q1 + 1..].find('"') {
                                                let ic = &trimmed[q1 + 1..q1 + 1 + q2];
                                                if !POPUP_VISIBLE.load(Ordering::Relaxed) {
                                                    if let Ok(mut st) = tracker_state().lock() {
                                                        st.active_ic_path = ic.to_string();
                                                        st.has_active_text_cursor = true;
                                                        if let Some(&rect) = st.ic_locations.get(ic)
                                                        {
                                                            st.text_cursor_rect = Some(rect);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        pending_member = None;
                                    } else if member == "SetCursorLocation"
                                        || member == "SetCursorLocationRelative"
                                    {
                                        if let Some(num_str) = trimmed.strip_prefix("int32 ") {
                                            if let Ok(val) = num_str.trim().parse::<i32>() {
                                                int_args.push(val);
                                                if int_args.len() == 4 {
                                                    let (x, y, w, h) = (
                                                        int_args[0],
                                                        int_args[1],
                                                        int_args[2],
                                                        int_args[3],
                                                    );
                                                    if !POPUP_VISIBLE.load(Ordering::Relaxed) {
                                                        if let Ok(mut st) = tracker_state().lock() {
                                                            if x > 0 || y > 0 {
                                                                let rect = (
                                                                    x,
                                                                    y,
                                                                    w.max(2),
                                                                    h.clamp(12, 120),
                                                                );
                                                                st.text_cursor_rect = Some(rect);
                                                                st.has_active_text_cursor = true;
                                                                let key = if pending_path
                                                                    .contains("InputContext_")
                                                                {
                                                                    pending_path.clone()
                                                                } else {
                                                                    st.active_ic_path.clone()
                                                                };
                                                                if !key.is_empty() {
                                                                    st.ic_locations
                                                                        .insert(key, rect);
                                                                }
                                                            }
                                                        }
                                                    } else if x > 0 || y > 0 {
                                                        // If popup is visible and the user clicked a different cursor position in the external editor
                                                        let hide_on_outside = storage_ibus
                                                            .data
                                                            .lock()
                                                            .map(|g| g.settings.hide_on_blur)
                                                            .unwrap_or(true);
                                                        if hide_on_outside {
                                                            let moved_outside = tracker_state()
                                                                .lock()
                                                                .ok()
                                                                .map(|mut st| {
                                                                    let show_ok = st
                                                                        .last_show_instant
                                                                        .map(|t| {
                                                                            t.elapsed()
                                                                                >= Duration::from_millis(650)
                                                                        })
                                                                        .unwrap_or(true);
                                                                    let commit_ok = st
                                                                        .last_commit_instant
                                                                        .map(|t| {
                                                                            t.elapsed()
                                                                                >= Duration::from_millis(550)
                                                                        })
                                                                        .unwrap_or(true);
                                                                    let jumped = st
                                                                        .shown_cursor_rect
                                                                        .map(|(sx, sy, _, _)| {
                                                                            (x - sx).abs() > 8
                                                                                || (y - sy).abs()
                                                                                    > 8
                                                                        })
                                                                        .unwrap_or(true);
                                                                    if show_ok && commit_ok && jumped
                                                                    {
                                                                        let rect = (
                                                                            x,
                                                                            y,
                                                                            w.max(2),
                                                                            h.clamp(12, 120),
                                                                        );
                                                                        st.text_cursor_rect =
                                                                            Some(rect);
                                                                        st.has_active_text_cursor =
                                                                            true;
                                                                        true
                                                                    } else {
                                                                        false
                                                                    }
                                                                })
                                                                .unwrap_or(false);
                                                            if moved_outside
                                                                && !is_window_dragging_recently()
                                                                && !is_pointer_inside_unblip_window(
                                                                    &app_ibus, 18,
                                                                )
                                                            {
                                                                hide_popup(&app_ibus);
                                                            }
                                                        }
                                                    }
                                                    pending_member = None;
                                                    int_args.clear();
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        let _ = child.wait();
                    }
                }
                thread::sleep(Duration::from_secs(2));
            }
        });

        // Thread 3: AT-SPI2 Wayland/X11 outside-click & external-window-activation detector
        thread::spawn(move || {
            loop {
                if let Ok(mut child) = Command::new("python3")
                    .args(["-u", "-c", ATSPI_OUTSIDE_CLICK_PY])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    if let Some(stdout) = child.stdout.take() {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines().flatten() {
                            if line.trim() == "OUTSIDE_EVENT"
                                && should_dismiss_on_outside_event(&app_atspi, &storage_atspi, 650)
                            {
                                hide_popup(&app_atspi);
                            }
                        }
                    }
                    let _ = child.wait();
                }
                thread::sleep(Duration::from_secs(3));
            }
        });
    });
}

#[derive(Debug, Clone, Copy)]
pub struct CursorAndScreen {
    pub cursor_x: i32,
    pub cursor_y: i32,
    pub screen_width: i32,
    pub screen_height: i32,
    pub button1_pressed: bool,
}

/// Queries X11/XWayland for `_NET_ACTIVE_WINDOW` on the root window.
pub fn query_x11_active_window() -> Option<u64> {
    ensure_safe_x11_error_handler();
    unsafe {
        let display = xlib::XOpenDisplay(ptr::null());
        if display.is_null() {
            return None;
        }
        let screen = xlib::XDefaultScreen(display);
        let root = xlib::XRootWindow(display, screen);
        let prop_name = b"_NET_ACTIVE_WINDOW\0";
        let atom = xlib::XInternAtom(display, prop_name.as_ptr() as *const i8, 1);
        let mut result = None;
        if atom != 0 {
            let mut actual_type: xlib::Atom = 0;
            let mut actual_format: i32 = 0;
            let mut nitems: u64 = 0;
            let mut bytes_after: u64 = 0;
            let mut prop: *mut u8 = ptr::null_mut();
            let status = xlib::XGetWindowProperty(
                display,
                root,
                atom,
                0,
                1,
                0,
                xlib::XA_WINDOW,
                &mut actual_type,
                &mut actual_format,
                &mut nitems,
                &mut bytes_after,
                &mut prop,
            );
            if status == 0 && !prop.is_null() && nitems >= 1 {
                let win_id = *(prop as *const u64);
                result = Some(win_id);
            }
            if !prop.is_null() {
                xlib::XFree(prop as *mut _);
            }
        }
        xlib::XCloseDisplay(display);
        result
    }
}

/// Queries X11/XWayland for current global pointer coordinates, screen size, and mouse button mask.
pub fn query_x11_pointer() -> Option<CursorAndScreen> {
    ensure_safe_x11_error_handler();
    unsafe {
        let display = xlib::XOpenDisplay(ptr::null());
        if display.is_null() {
            return None;
        }

        let screen = xlib::XDefaultScreen(display);
        let root = xlib::XRootWindow(display, screen);
        let screen_width = xlib::XDisplayWidth(display, screen);
        let screen_height = xlib::XDisplayHeight(display, screen);

        let mut root_return: xlib::Window = 0;
        let mut child_return: xlib::Window = 0;
        let mut root_x: i32 = 0;
        let mut root_y: i32 = 0;
        let mut win_x: i32 = 0;
        let mut win_y: i32 = 0;
        let mut mask_return: u32 = 0;

        let ok = xlib::XQueryPointer(
            display,
            root,
            &mut root_return,
            &mut child_return,
            &mut root_x,
            &mut root_y,
            &mut win_x,
            &mut win_y,
            &mut mask_return,
        );

        xlib::XCloseDisplay(display);

        if ok != 0 {
            let any_button_mask =
                (xlib::Button1Mask | xlib::Button2Mask | xlib::Button3Mask) as u32;
            let button1_pressed = (mask_return & any_button_mask) != 0;
            Some(CursorAndScreen {
                cursor_x: root_x,
                cursor_y: root_y,
                screen_width: screen_width.max(800),
                screen_height: screen_height.max(600),
                button1_pressed,
            })
        } else {
            None
        }
    }
}

/// Computes popup (x, y) near a point or caret rectangle while ensuring the popup never clips off screen edges.
pub fn compute_clamped_position(
    cursor_x: i32,
    cursor_y: i32,
    screen_w: i32,
    screen_h: i32,
    win_w: i32,
    win_h: i32,
) -> (i32, i32) {
    compute_clamped_position_for_rect(cursor_x, cursor_y, 2, 14, screen_w, screen_h, win_w, win_h)
}

/// Computes popup (x, y) close to an active text cursor / input box rectangle `(rect_x, rect_y, rect_w, rect_h)`
/// while guaranteeing an exclusion zone around the entire input box so the popup never overlaps it.
pub fn compute_clamped_position_for_rect(
    rect_x: i32,
    rect_y: i32,
    rect_w: i32,
    rect_h: i32,
    screen_w: i32,
    screen_h: i32,
    win_w: i32,
    win_h: i32,
) -> (i32, i32) {
    let margin = 12;
    let caret_h = rect_h.clamp(16, 80);

    // Input box exclusion zone around the text caret:
    // 42px above the caret top and 52px below the caret bottom so multi-line inputs,
    // borders, and chat box toolbars are never covered by the popup.
    let box_top = rect_y - 42;
    let box_bottom = rect_y + caret_h + 52;

    let mut x = rect_x;
    if x + win_w + margin > screen_w {
        x = (screen_w - win_w - margin).max(margin);
    }
    if x < margin {
        x = margin;
    }

    let space_above = box_top - margin;
    let space_below = (screen_h - margin) - box_bottom;

    let y = if space_below >= win_h && (space_below >= space_above || space_above < win_h) {
        // Place completely below the input box
        box_bottom
    } else if space_above >= win_h {
        // Place completely above the input box
        box_top - win_h
    } else {
        // Very small screen where neither full vertical clearance fits:
        // shift horizontally beside the caret so we still don't cover the input area
        let right_x = rect_x + rect_w.max(24) + 32;
        let left_x = rect_x - win_w - 32;
        if right_x + win_w + margin <= screen_w {
            x = right_x;
        } else if left_x >= margin {
            x = left_x;
        }
        if space_below >= space_above {
            (screen_h - win_h - margin).max(rect_y + caret_h + 16)
        } else {
            margin.min((rect_y - win_h - 16).max(0))
        }
    };

    (x, y)
}

pub fn show_popup_near_cursor(app: &AppHandle, near_cursor: bool) {
    let _ = ensure_ibus_committer();

    if let Some(window) = app.get_webview_window("main") {
        // Ensure clicking emojis or cards in Unblip does NOT steal keyboard focus from the user's input box!
        if let Ok(gtk_win) = window.gtk_window() {
            gtk_win.set_accept_focus(false);
            gtk_win.set_focus_on_map(false);
        }

        // If window is currently in maximized full-size mode, show it centered without resetting size
        if POPUP_MAXIMIZED.load(Ordering::Relaxed) {
            if let Ok(mut st) = tracker_state().lock() {
                st.last_show_instant = Some(Instant::now());
            }
            POPUP_VISIBLE.store(true, Ordering::Relaxed);
            let _ = window.show();
            let _ = window.set_always_on_top(true);
            let _ = app.emit("popup-shown", ());
            return;
        }

        let x11_info = query_x11_pointer();
        let (screen_w, screen_h) = x11_info
            .map(|i| (i.screen_width, i.screen_height))
            .unwrap_or((1920, 1080));

        // Save current window position as fallback last_appeared_spot if not set yet
        if let Ok(pos) = window.outer_position() {
            if let Ok(mut st) = tracker_state().lock() {
                if st.last_appeared_spot.is_none() && pos.x > 0 && pos.y > 32 {
                    st.last_appeared_spot = Some((pos.x, pos.y));
                }
            }
        }

        let (target_x, target_y) = {
            let mut st = tracker_state().lock().unwrap();
            st.last_show_instant = Some(Instant::now());
            st.shown_with_active_cursor = st.has_active_text_cursor;
            st.shown_ic_path = st.active_ic_path.clone();
            st.shown_cursor_rect = st.text_cursor_rect;

            if !near_cursor {
                st.last_appeared_spot.unwrap_or((
                    (screen_w - POPUP_WIDTH) / 2,
                    (screen_h - POPUP_HEIGHT) / 2,
                ))
            } else if st.has_active_text_cursor && st.text_cursor_rect.is_some() {
                // Active text cursor / input box is present -> place close to the input box!
                let (cx, cy, cw, ch) = st.text_cursor_rect.unwrap();
                let computed = compute_clamped_position_for_rect(
                    cx,
                    cy,
                    cw,
                    ch,
                    screen_w,
                    screen_h,
                    POPUP_WIDTH,
                    POPUP_HEIGHT,
                );
                st.last_appeared_spot = Some(computed);
                computed
            } else if let Some(last_spot) = st.last_appeared_spot {
                // Without active text cursor -> appear in the last appeared spot!
                last_spot
            } else if let Some(info) = x11_info {
                let computed = compute_clamped_position(
                    info.cursor_x,
                    info.cursor_y,
                    info.screen_width,
                    info.screen_height,
                    POPUP_WIDTH,
                    POPUP_HEIGHT,
                );
                st.last_appeared_spot = Some(computed);
                computed
            } else {
                let fallback = ((screen_w - POPUP_WIDTH) / 2, (screen_h - POPUP_HEIGHT) / 2);
                st.last_appeared_spot = Some(fallback);
                fallback
            }
        };

        POPUP_VISIBLE.store(true, Ordering::Relaxed);

        let _ = window.set_position(Position::Physical(PhysicalPosition {
            x: target_x,
            y: target_y,
        }));
        let _ = window.show();
        let _ = window.set_always_on_top(true);
        let _ = window.set_position(Position::Physical(PhysicalPosition {
            x: target_x,
            y: target_y,
        }));

        // Apply position again at +25ms and +65ms after show() so GNOME Mutter / XWayland
        // always honors the move even after mapping a previously hidden/dragged window.
        let win_clone = window.clone();
        thread::spawn(move || {
            for delay_ms in [25, 65] {
                thread::sleep(Duration::from_millis(delay_ms));
                let _ = win_clone.set_position(Position::Physical(PhysicalPosition {
                    x: target_x,
                    y: target_y,
                }));
            }
        });

        let _ = app.emit("popup-shown", ());
    }
}

pub fn hide_popup(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if !POPUP_MAXIMIZED.load(Ordering::Relaxed) {
            if let Ok(pos) = window.outer_position() {
                // Only update last_appeared_spot from outer_position() if GTK reports a valid non-panel Y coordinate
                if pos.x > 0 && pos.y > 32 {
                    if let Ok(mut st) = tracker_state().lock() {
                        st.last_appeared_spot = Some((pos.x, pos.y));
                        st.last_hide_instant = Some(Instant::now());
                    }
                } else if let Ok(mut st) = tracker_state().lock() {
                    st.last_hide_instant = Some(Instant::now());
                }
            }
        } else if let Ok(mut st) = tracker_state().lock() {
            st.last_hide_instant = Some(Instant::now());
        }
        POPUP_VISIBLE.store(false, Ordering::Relaxed);
        POINTER_INSIDE_WEBVIEW.store(false, Ordering::Relaxed);
        DRAG_IN_PROGRESS.store(false, Ordering::Relaxed);
        let _ = window.hide();
    }
}

pub fn toggle_popup(app: &AppHandle, near_cursor: bool) {
    // Debounce duplicate shortcut triggers (e.g., X11 GlobalShortcut + GNOME gsettings --toggle firing for the same keypress)
    if let Ok(mut st) = tracker_state().lock() {
        let too_soon = st
            .last_toggle_instant
            .map(|t| t.elapsed() < Duration::from_millis(350))
            .unwrap_or(false);
        if too_soon {
            return;
        }
        st.last_toggle_instant = Some(Instant::now());
    }

    if let Some(window) = app.get_webview_window("main") {
        let is_visible = window.is_visible().unwrap_or(false);
        if is_visible {
            hide_popup(app);
        } else {
            show_popup_near_cursor(app, near_cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_clamped_position_inside_screen() {
        let (x, y) = compute_clamped_position(300, 200, 1920, 1080, 368, 440);
        assert_eq!(x, 300);
        // Should be placed completely below the input box exclusion zone (200 + 16 + 52 = 268)
        assert_eq!(y, 268);
        assert!(y >= 200 + 16 + 52);
    }

    #[test]
    fn test_compute_clamped_position_bottom_input_box_flips_above() {
        // E.g. WhatsApp or Discord chat input box near bottom of 1920x1080 screen (y=980)
        let (x, y) = compute_clamped_position_for_rect(600, 980, 300, 28, 1920, 1080, 368, 440);
        assert_eq!(x, 600);
        // Should flip directly above the input box exclusion zone (980 - 42 - 440 = 498)
        assert_eq!(y, 498);
        assert!(y + 440 <= 980 - 42);
    }

    #[test]
    fn test_never_overlaps_input_box_across_entire_screen_height() {
        // Test every possible vertical input box coordinate y from 30 to 1050 on 1920x1080
        for caret_y in (30..=1050).step_by(10) {
            let caret_h = 22;
            let (_x, popup_y) =
                compute_clamped_position_for_rect(500, caret_y, 10, caret_h, 1920, 1080, 368, 440);
            let popup_top = popup_y;
            let popup_bottom = popup_y + 440;
            let input_box_top = caret_y - 42;
            let input_box_bottom = caret_y + caret_h + 52;
            let overlaps = popup_top < input_box_bottom && popup_bottom > input_box_top;
            assert!(
                !overlaps,
                "Popup [{popup_top}..{popup_bottom}] overlapped input box [{input_box_top}..{input_box_bottom}] at caret_y={caret_y}"
            );
        }
    }

    #[test]
    fn test_compute_clamped_position_bottom_right_edge() {
        let (x, y) = compute_clamped_position(1850, 980, 1920, 1080, 368, 440);
        assert!(x + 368 <= 1920);
        assert!(y + 440 <= 1080);
        assert!(x >= 12);
        assert!(y >= 12);
    }
}
