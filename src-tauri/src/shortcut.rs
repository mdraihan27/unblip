use crate::storage::SharedStorage;
use crate::window::toggle_popup;
use std::env;
use std::fs;
use std::process::Command;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// Converts human-readable shortcut like "Super+V" or "Ctrl+Shift+V" to GNOME gsettings accelerator format "<Super>v" / "<Control><Shift>v".
pub fn to_gnome_accelerator(shortcut: &str) -> String {
    let parts: Vec<&str> = shortcut.split('+').map(|s| s.trim()).collect();
    let mut mods = String::new();
    let mut key = String::new();

    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "super" | "meta" | "win" | "cmd" | "commandorcontrol" => mods.push_str("<Super>"),
            "ctrl" | "control" => mods.push_str("<Control>"),
            "alt" | "option" => mods.push_str("<Alt>"),
            "shift" => mods.push_str("<Shift>"),
            other => {
                key = other.to_string();
            }
        }
    }

    if key.is_empty() {
        key = "v".to_string();
    }

    format!("{}{}", mods, key)
}

/// Normalizes a user shortcut for `tauri-plugin-global-shortcut` (e.g., "Super+V" -> "Super+KeyV").
pub fn to_tauri_shortcut(shortcut: &str) -> String {
    let parts: Vec<&str> = shortcut.split('+').map(|s| s.trim()).collect();
    let mut out: Vec<String> = Vec::new();

    for part in parts {
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "super" | "meta" | "win" => out.push("Super".to_string()),
            "ctrl" | "control" => out.push("Control".to_string()),
            "alt" => out.push("Alt".to_string()),
            "shift" => out.push("Shift".to_string()),
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphabetic()) => {
                out.push(format!("Key{}", k.to_ascii_uppercase()));
            }
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_digit()) => {
                out.push(format!("Digit{}", k));
            }
            other => out.push(other.to_uppercase()),
        }
    }

    out.join("+")
}

pub fn register_shortcut(app: &AppHandle, storage: &SharedStorage, shortcut_str: &str) -> Result<String, String> {
    let tauri_sc = to_tauri_shortcut(shortcut_str);

    // 1. Register via Tauri GlobalShortcut plugin (works on X11 & XWayland windows)
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    let storage_clone = storage.clone();
    let _ = gs.on_shortcut(tauri_sc.as_str(), move |app_handle, _sc, event| {
        if event.state == ShortcutState::Pressed {
            let near_cursor = storage_clone
                .data
                .lock()
                .map(|d| d.settings.show_near_cursor)
                .unwrap_or(true);
            toggle_popup(app_handle, near_cursor);
        }
    });

    // 2. Also register in GNOME gsettings custom-keybindings so it works globally on Ubuntu GNOME Wayland
    let _ = register_gnome_custom_keybinding(shortcut_str);

    Ok(shortcut_str.to_string())
}

fn register_gnome_custom_keybinding(shortcut_str: &str) -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let exe_str = exe.to_string_lossy().to_string();
    let cmd_str = format!("{} --toggle", exe_str);
    let accel = to_gnome_accelerator(shortcut_str);

    let custom_path = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/unblip/";
    let schema_list = "org.gnome.settings-daemon.plugins.media-keys";
    let schema_binding = format!(
        "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{}",
        custom_path
    );

    // Read existing custom-keybindings array
    if let Ok(output) = Command::new("gsettings")
        .args(["get", schema_list, "custom-keybindings"])
        .output()
    {
        if output.status.success() {
            let current = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let updated_list = if current == "@as []" || current == "[]" || current.is_empty() {
                format!("['{}']", custom_path)
            } else if !current.contains(custom_path) && current.ends_with(']') {
                let prefix = &current[..current.len() - 1];
                format!("{}, '{}']", prefix, custom_path)
            } else {
                current
            };

            let _ = Command::new("gsettings")
                .args(["set", schema_list, "custom-keybindings", &updated_list])
                .status();

            let _ = Command::new("gsettings")
                .args(["set", &schema_binding, "name", "Unblip Clipboard Manager"])
                .status();
            let _ = Command::new("gsettings")
                .args(["set", &schema_binding, "command", &cmd_str])
                .status();
            let _ = Command::new("gsettings")
                .args(["set", &schema_binding, "binding", &accel])
                .status();
        }
    }

    Ok(())
}

pub fn sync_autostart_desktop_file(enabled: bool) -> Result<(), String> {
    let config_dir = dirs::config_dir().ok_or_else(|| "No config dir".to_string())?;
    let autostart_dir = config_dir.join("autostart");
    let _ = fs::create_dir_all(&autostart_dir);
    let desktop_file = autostart_dir.join("unblip.desktop");

    if enabled {
        let exe = env::current_exe().map_err(|e| e.to_string())?;
        let content = format!(
            "[Desktop Entry]\n\
            Type=Application\n\
            Name=Unblip\n\
            Comment=Windows 11 Clipboard & Emoji Manager for Linux\n\
            Exec={} --background\n\
            Icon=unblip\n\
            Terminal=false\n\
            Categories=Utility;\n\
            StartupNotify=false\n\
            X-GNOME-Autostart-enabled=true\n",
            exe.to_string_lossy()
        );
        fs::write(&desktop_file, content).map_err(|e| e.to_string())?;
    } else if desktop_file.exists() {
        let _ = fs::remove_file(&desktop_file);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_gnome_accelerator() {
        assert_eq!(to_gnome_accelerator("Super+V"), "<Super>v");
        assert_eq!(
            to_gnome_accelerator("Ctrl+Shift+V"),
            "<Control><Shift>v"
        );
        assert_eq!(to_gnome_accelerator("Alt+C"), "<Alt>c");
    }

    #[test]
    fn test_to_tauri_shortcut() {
        assert_eq!(to_tauri_shortcut("Super+V"), "Super+KeyV");
        assert_eq!(to_tauri_shortcut("Ctrl+Shift+V"), "Control+Shift+KeyV");
    }
}

