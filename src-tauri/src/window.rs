use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
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
        }
    }
}

static TRACKER: OnceLock<Mutex<CursorTrackingState>> = OnceLock::new();

fn tracker_state() -> &'static Mutex<CursorTrackingState> {
    TRACKER.get_or_init(|| Mutex::new(CursorTrackingState::default()))
}

/// Updates the remembered window position when the user manually drags the popup window.
pub fn record_window_moved(x: i32, y: i32) {
    if POPUP_VISIBLE.load(Ordering::Relaxed) && !POPUP_MAXIMIZED.load(Ordering::Relaxed) {
        if let Ok(mut st) = tracker_state().lock() {
            // Only record manual drag moves after initial show settle (120ms)
            let settled = st
                .last_show_instant
                .map(|t| t.elapsed() > Duration::from_millis(120))
                .unwrap_or(true);
            if settled && x >= 0 && y >= 0 {
                st.last_appeared_spot = Some((x, y));
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

/// Starts a background listener on the user's IBus D-Bus socket to track real-time
/// text input box focus (`FocusIn` / `FocusOut`) and text cursor coordinates (`SetCursorLocation`)
/// across all Wayland and X11 applications (Chrome, VS Code, Terminal, GTK, Qt, etc.).
pub fn start_ibus_cursor_tracker() {
    IBUS_TRACKER_INIT.call_once(|| {
        thread::spawn(|| {
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
                                        match member {
                                            "SetCursorLocation" | "SetCursorLocationRelative" => {
                                                pending_member = Some(member.to_string());
                                            }
                                            "FocusIn" => {
                                                if !POPUP_VISIBLE.load(Ordering::Relaxed) {
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
                                                }
                                                pending_member = Some("FocusIn".to_string());
                                            }
                                            "FocusOut" | "DestroyContext" => {
                                                if !POPUP_VISIBLE.load(Ordering::Relaxed) {
                                                    if let Ok(mut st) = tracker_state().lock() {
                                                        // Ignore FocusOut within 450ms of popup showing or hiding
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
                                                            // Only update rectangle if coordinates are non-zero screen coordinates
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
            let button1_pressed = (mask_return & (xlib::Button1Mask as u32)) != 0;
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
    start_ibus_cursor_tracker();

    if let Some(window) = app.get_webview_window("main") {
        // If window is currently in maximized full-size mode, show it centered without resetting size
        if POPUP_MAXIMIZED.load(Ordering::Relaxed) {
            POPUP_VISIBLE.store(true, Ordering::Relaxed);
            let _ = window.show();
            let _ = window.set_always_on_top(true);
            let _ = window.set_focus();
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
                if st.last_appeared_spot.is_none() && (pos.x > 0 || pos.y > 0) {
                    st.last_appeared_spot = Some((pos.x, pos.y));
                }
            }
        }

        let (target_x, target_y) = {
            let mut st = tracker_state().lock().unwrap();
            st.last_show_instant = Some(Instant::now());

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
        let _ = window.set_focus();
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
                if pos.x >= 0 && pos.y >= 0 {
                    if let Ok(mut st) = tracker_state().lock() {
                        st.last_appeared_spot = Some((pos.x, pos.y));
                        st.last_hide_instant = Some(Instant::now());
                    }
                }
            }
        } else if let Ok(mut st) = tracker_state().lock() {
            st.last_hide_instant = Some(Instant::now());
        }
        POPUP_VISIBLE.store(false, Ordering::Relaxed);
        let _ = window.hide();
    }
}

pub fn toggle_popup(app: &AppHandle, near_cursor: bool) {
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
