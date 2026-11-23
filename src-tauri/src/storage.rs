use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Text,
    Url,
    Color,
    Code,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardItem {
    pub id: String,
    pub content_type: ContentType,
    pub content: String,
    pub preview: String,
    pub image_dimensions: Option<(u32, u32)>,
    pub char_count: usize,
    pub pinned: bool,
    pub created_at: DateTime<Utc>,
    pub created_at_display: String,
    pub last_clicked_at: Option<DateTime<Utc>>,
    pub last_clicked_display: Option<String>,
    pub click_count: u64,
    pub source_kind: String, // "clipboard" | "selection"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrequentItem {
    pub symbol: String,
    pub name: String,
    pub kind: String, // "emoji" | "kaomoji" | "symbol"
    pub use_count: u64,
    pub last_used_at: DateTime<Utc>,
    pub last_used_display: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub history_enabled: bool,
    pub shortcut: String,
    pub auto_copy_selection: bool,
    pub sync_selection_to_clipboard: bool,
    pub auto_paste: bool,
    pub theme: String, // "light" | "dark" | "system"
    pub show_near_cursor: bool,
    pub hide_on_blur: bool,
    pub autostart: bool,
    pub play_sound: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            history_enabled: true,
            shortcut: "Super+V".to_string(),
            auto_copy_selection: false,
            sync_selection_to_clipboard: false,
            auto_paste: true,
            theme: "light".to_string(),
            show_near_cursor: true,
            hide_on_blur: false,
            autostart: true,
            play_sound: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentData {
    pub settings: AppSettings,
    pub items: Vec<ClipboardItem>,
    pub frequent_items: Vec<FrequentItem>,
}

impl Default for PersistentData {
    fn default() -> Self {
        Self {
            settings: AppSettings::default(),
            items: Vec::new(),
            frequent_items: Vec::new(),
        }
    }
}

pub struct StorageState {
    pub data: Mutex<PersistentData>,
    pub file_path: PathBuf,
    pub last_clipboard_text: Mutex<String>,
    pub last_selection_text: Mutex<String>,
    pub last_image_hash: Mutex<u64>,
    pub ignore_next_clipboard: Mutex<bool>,
}

pub type SharedStorage = Arc<StorageState>;

pub fn format_local_timestamp(dt: DateTime<Utc>) -> String {
    let local: DateTime<Local> = DateTime::from(dt);
    local.format("%b %-d, %-I:%M %p").to_string()
}

pub fn classify_text(text: &str) -> ContentType {
    let trimmed = text.trim();

    if is_color_string(trimmed) {
        return ContentType::Color;
    }

    if (trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("ftp://")
        || trimmed.starts_with("mailto:"))
        && !trimmed.contains(char::is_whitespace)
    {
        return ContentType::Url;
    }

    if is_likely_code(trimmed) {
        return ContentType::Code;
    }

    ContentType::Text
}

pub fn is_color_string(s: &str) -> bool {
    if let Some(hex) = s.strip_prefix('#') {
        let len = hex.len();
        if (len == 3 || len == 4 || len == 6 || len == 8)
            && hex.chars().all(|c| c.is_ascii_hexdigit())
        {
            return true;
        }
    }
    let lower = s.to_ascii_lowercase();
    if (lower.starts_with("rgb(")
        || lower.starts_with("rgba(")
        || lower.starts_with("hsl(")
        || lower.starts_with("hsla("))
        && lower.ends_with(')')
        && lower.len() <= 48
    {
        return true;
    }
    false
}

fn is_likely_code(s: &str) -> bool {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() >= 2 {
        let code_indicators = [
            "fn ",
            "pub fn ",
            "let ",
            "const ",
            "import ",
            "export ",
            "function ",
            "class ",
            "def ",
            "return ",
            "#include",
            "SELECT ",
            "FROM ",
            "if (",
            "for (",
            "while (",
            "=>",
            "::",
            "println!",
            "console.log",
        ];
        let hits = code_indicators
            .iter()
            .filter(|kw| s.contains(*kw))
            .count();
        if hits >= 2 || (hits >= 1 && (s.contains('{') || s.contains(';'))) {
            return true;
        }
    }
    false
}

impl StorageState {
    pub fn new(custom_path: Option<PathBuf>) -> SharedStorage {
        let file_path = custom_path.unwrap_or_else(|| {
            let base = dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("unblip");
            let _ = fs::create_dir_all(&base);
            base.join("store.json")
        });

        if let Some(parent) = file_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mut data = if file_path.exists() {
            match fs::read_to_string(&file_path) {
                Ok(raw) => serde_json::from_str::<PersistentData>(&raw).unwrap_or_else(|_| {
                    let bak_path = file_path.with_extension("json.bak");
                    fs::read_to_string(&bak_path)
                        .ok()
                        .and_then(|b| serde_json::from_str::<PersistentData>(&b).ok())
                        .unwrap_or_default()
                }),
                Err(_) => PersistentData::default(),
            }
        } else {
            PersistentData::default()
        };

        // Normalize all loaded display timestamps to the unified "Jan 6, 26 3:45 PM" format
        for item in &mut data.items {
            item.created_at_display = format_local_timestamp(item.created_at);
            if let Some(clicked_at) = item.last_clicked_at {
                item.last_clicked_display = Some(format_local_timestamp(clicked_at));
            }
        }
        for freq in &mut data.frequent_items {
            freq.last_used_display = format_local_timestamp(freq.last_used_at);
        }

        Arc::new(Self {
            data: Mutex::new(data),
            file_path,
            last_clipboard_text: Mutex::new(String::new()),
            last_selection_text: Mutex::new(String::new()),
            last_image_hash: Mutex::new(0),
            ignore_next_clipboard: Mutex::new(false),
        })
    }

    pub fn save_locked(&self, data: &PersistentData) {
        if let Ok(json) = serde_json::to_string_pretty(data) {
            let tmp_path = self.file_path.with_extension("json.tmp");
            let bak_path = self.file_path.with_extension("json.bak");
            if fs::write(&tmp_path, &json).is_ok() {
                if self.file_path.exists() {
                    let _ = fs::copy(&self.file_path, &bak_path);
                }
                let _ = fs::rename(&tmp_path, &self.file_path);
            } else {
                let _ = fs::write(&self.file_path, &json);
            }
        }
    }

    pub fn add_text_item(&self, text: String, source_kind: &str) -> Option<ClipboardItem> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }

        let mut guard = self.data.lock().ok()?;
        if !guard.settings.history_enabled {
            return None;
        }

        if let Some(top) = guard.items.first() {
            if top.content_type != ContentType::Image && top.content == text {
                return None;
            }
        }

        let now = Utc::now();
        let now_display = format_local_timestamp(now);

        if let Some(pos) = guard
            .items
            .iter()
            .position(|item| item.content_type != ContentType::Image && item.content == text)
        {
            let mut existing = guard.items.remove(pos);
            existing.created_at = now;
            existing.created_at_display = now_display;
            existing.source_kind = source_kind.to_string();
            let cloned = existing.clone();
            guard.items.insert(0, existing);
            self.save_locked(&guard);
            return Some(cloned);
        }

        let content_type = classify_text(&text);
        let preview: String = text.chars().take(280).collect();
        let char_count = text.chars().count();

        let item = ClipboardItem {
            id: Uuid::new_v4().to_string(),
            content_type,
            content: text,
            preview,
            image_dimensions: None,
            char_count,
            pinned: false,
            created_at: now,
            created_at_display: now_display,
            last_clicked_at: None,
            last_clicked_display: None,
            click_count: 0,
            source_kind: source_kind.to_string(),
        };

        guard.items.insert(0, item.clone());
        self.save_locked(&guard);
        Some(item)
    }

    pub fn add_image_item(
        &self,
        data_url: String,
        width: u32,
        height: u32,
    ) -> Option<ClipboardItem> {
        let mut guard = self.data.lock().ok()?;
        if !guard.settings.history_enabled {
            return None;
        }

        if let Some(top) = guard.items.first() {
            if top.content_type == ContentType::Image && top.content == data_url {
                return None;
            }
        }

        let now = Utc::now();
        let now_display = format_local_timestamp(now);

        let item = ClipboardItem {
            id: Uuid::new_v4().to_string(),
            content_type: ContentType::Image,
            content: data_url,
            preview: format!("Image ({} × {} px)", width, height),
            image_dimensions: Some((width, height)),
            char_count: 0,
            pinned: false,
            created_at: now,
            created_at_display: now_display,
            last_clicked_at: None,
            last_clicked_display: None,
            click_count: 0,
            source_kind: "clipboard".to_string(),
        };

        guard.items.insert(0, item.clone());
        self.save_locked(&guard);
        Some(item)
    }

    pub fn record_item_clicked(&self, id: &str) -> Option<ClipboardItem> {
        let mut guard = self.data.lock().ok()?;
        let now = Utc::now();
        let now_display = format_local_timestamp(now);

        let item = guard.items.iter_mut().find(|i| i.id == id)?;
        item.last_clicked_at = Some(now);
        item.last_clicked_display = Some(now_display);
        item.click_count += 1;
        let updated = item.clone();
        self.save_locked(&guard);
        Some(updated)
    }

    pub fn toggle_pin(&self, id: &str) -> Option<bool> {
        let mut guard = self.data.lock().ok()?;
        let item = guard.items.iter_mut().find(|i| i.id == id)?;
        item.pinned = !item.pinned;
        let new_state = item.pinned;
        self.save_locked(&guard);
        Some(new_state)
    }

    pub fn delete_item(&self, id: &str) -> bool {
        if let Ok(mut guard) = self.data.lock() {
            let before = guard.items.len();
            guard.items.retain(|i| i.id != id);
            let removed = guard.items.len() < before;
            if removed {
                self.save_locked(&guard);
            }
            removed
        } else {
            false
        }
    }

    pub fn clear_all(&self, include_pinned: bool) -> usize {
        if let Ok(mut guard) = self.data.lock() {
            let before = guard.items.len();
            if include_pinned {
                guard.items.clear();
            } else {
                guard.items.retain(|i| i.pinned);
            }
            let removed = before.saturating_sub(guard.items.len());
            self.save_locked(&guard);
            removed
        } else {
            0
        }
    }

    pub fn record_frequent_symbol(
        &self,
        symbol: String,
        name: String,
        kind: String,
    ) -> Vec<FrequentItem> {
        if let Ok(mut guard) = self.data.lock() {
            let now = Utc::now();
            let now_display = format_local_timestamp(now);
            if let Some(existing) = guard
                .frequent_items
                .iter_mut()
                .find(|f| f.symbol == symbol)
            {
                existing.use_count += 1;
                existing.last_used_at = now;
                existing.last_used_display = now_display;
            } else {
                guard.frequent_items.push(FrequentItem {
                    symbol,
                    name,
                    kind,
                    use_count: 1,
                    last_used_at: now,
                    last_used_display: now_display,
                });
            }

            guard.frequent_items.sort_by(|a, b| {
                b.use_count
                    .cmp(&a.use_count)
                    .then_with(|| b.last_used_at.cmp(&a.last_used_at))
            });

            if guard.frequent_items.len() > 64 {
                guard.frequent_items.truncate(64);
            }

            self.save_locked(&guard);
            guard.frequent_items.clone()
        } else {
            Vec::new()
        }
    }

    pub fn clear_frequent_items(&self) -> usize {
        if let Ok(mut guard) = self.data.lock() {
            let count = guard.frequent_items.len();
            guard.frequent_items.clear();
            self.save_locked(&guard);
            count
        } else {
            0
        }
    }

    pub fn update_settings(&self, new_settings: AppSettings) -> AppSettings {
        if let Ok(mut guard) = self.data.lock() {
            guard.settings = new_settings.clone();
            self.save_locked(&guard);
        }
        new_settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_store_path(test_name: &str) -> PathBuf {
        env::temp_dir().join(format!("unblip_test_{}_{}.json", test_name, Uuid::new_v4()))
    }

    #[test]
    fn test_classify_text_variants() {
        assert_eq!(classify_text("#0078D4"), ContentType::Color);
        assert_eq!(classify_text("rgb(255, 120, 0)"), ContentType::Color);
        assert_eq!(
            classify_text("https://tauri.app/v2/guide"),
            ContentType::Url
        );
        assert_eq!(
            classify_text("fn main() {\n    println!(\"hello\");\n}"),
            ContentType::Code
        );
        assert_eq!(
            classify_text("How to install Windows 11 on your PC"),
            ContentType::Text
        );
    }

    #[test]
    fn test_persistence_click_timestamp_pin_and_clear() {
        let path = temp_store_path("persistence");
        {
            let store = StorageState::new(Some(path.clone()));
            let item1 = store
                .add_text_item("First item".to_string(), "clipboard")
                .expect("should add item1");
            let item2 = store
                .add_text_item("Pinned item".to_string(), "clipboard")
                .expect("should add item2");

            let clicked = store
                .record_item_clicked(&item1.id)
                .expect("should record click");
            assert_eq!(clicked.click_count, 1);
            assert!(clicked.last_clicked_at.is_some());
            assert!(clicked.last_clicked_display.is_some());

            assert_eq!(store.toggle_pin(&item2.id), Some(true));

            let removed = store.clear_all(false);
            assert_eq!(removed, 1);
        }

        let reloaded = StorageState::new(Some(path.clone()));
        let guard = reloaded.data.lock().unwrap();
        assert_eq!(guard.items.len(), 1);
        assert_eq!(guard.items[0].content, "Pinned item");
        assert!(guard.items[0].pinned);

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("json.bak"));
    }

    #[test]
    fn test_frequent_emojis_ranking_and_clear() {
        let path = temp_store_path("emojis");
        let store = StorageState::new(Some(path.clone()));

        store.record_frequent_symbol("🚀".to_string(), "Rocket".to_string(), "emoji".to_string());
        store.record_frequent_symbol("🔥".to_string(), "Fire".to_string(), "emoji".to_string());
        let list =
            store.record_frequent_symbol("🔥".to_string(), "Fire".to_string(), "emoji".to_string());

        assert_eq!(list.len(), 2);
        assert_eq!(list[0].symbol, "🔥");
        assert_eq!(list[0].use_count, 2);
        assert_eq!(list[1].symbol, "🚀");
        assert_eq!(list[1].use_count, 1);

        assert_eq!(store.clear_frequent_items(), 2);
        assert!(store.data.lock().unwrap().frequent_items.is_empty());

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("json.bak"));
    }

    #[test]
    fn test_corrupted_json_backup_recovery_and_timestamp_format() {
        let path = temp_store_path("backup_recovery");
        {
            let store = StorageState::new(Some(path.clone()));
            let item = store
                .add_text_item("Backup test item".to_string(), "clipboard")
                .expect("should add item");
            assert!(item.created_at_display.contains(','));
            assert!(
                item.created_at_display.ends_with("AM")
                    || item.created_at_display.ends_with("PM")
            );
            store.add_text_item("Second item".to_string(), "clipboard");
        }

        // Corrupt primary JSON file and verify backup recovery
        fs::write(&path, "{ corrupted json !!!").unwrap();
        let recovered = StorageState::new(Some(path.clone()));
        let guard = recovered.data.lock().unwrap();
        assert_eq!(guard.items.len(), 1);
        assert_eq!(guard.items[0].content, "Backup test item");

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("json.bak"));
    }
}
