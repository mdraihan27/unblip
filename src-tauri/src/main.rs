#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::process::{Command, Stdio};
use unblip_lib::try_send_ipc_command;

fn spawn_detached_daemon(start_hidden: bool) -> bool {
    if let Ok(exe) = env::current_exe() {
        let flag = if start_hidden {
            "--daemon-hidden"
        } else {
            "--daemon-visible"
        };
        if Command::new(exe)
            .arg(flag)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return true;
        }
    }
    false
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let is_daemon_process = args
        .iter()
        .any(|a| a == "--daemon-visible" || a == "--daemon-hidden" || a == "--foreground");

    if !is_daemon_process {
        if args.iter().any(|a| a == "--toggle" || a == "-t") {
            if try_send_ipc_command("TOGGLE") {
                return;
            }
            if spawn_detached_daemon(false) {
                return;
            }
        } else if args.iter().any(|a| a == "--show") {
            if try_send_ipc_command("SHOW") {
                return;
            }
            if spawn_detached_daemon(false) {
                return;
            }
        } else if args.iter().any(|a| a == "--hide") {
            if try_send_ipc_command("HIDE") {
                return;
            }
            return;
        } else if args.iter().any(|a| a == "--background") {
            if try_send_ipc_command("HIDE") {
                return;
            }
            if spawn_detached_daemon(true) {
                return;
            }
        } else {
            // Default invocation (`unblip`): toggle if running, else spawn detached daemon and show immediately
            if try_send_ipc_command("TOGGLE") {
                return;
            }
            if spawn_detached_daemon(false) {
                return;
            }
        }
    }

    // Use X11/XWayland backend by default on Linux so window positioning near cursor (set_position),
    // XQueryPointer cursor tracking, primary selection monitoring, and XTest auto-paste work seamlessly
    // even under GNOME Wayland.
    if env::var("GDK_BACKEND").is_err() {
        env::set_var("GDK_BACKEND", "x11");
    }
    if env::var("GTK_IM_MODULE").is_err() {
        // Prevent Unblip's own popup window from creating an IBus InputContext or overwriting the active app's IBus cursor location
        env::set_var("GTK_IM_MODULE", "gtk-im-context-simple");
    }
    if env::var("WEBKIT_DISABLE_COMPOSITING_MODE").is_err() {
        env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    let start_hidden = args
        .iter()
        .any(|a| a == "--background" || a == "--daemon-hidden");
    unblip_lib::run(start_hidden);
}
