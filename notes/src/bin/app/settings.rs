//! What the window keeps across restarts: its size, the sidebar's widths in
//! each layout, the editor's zoom, and the tree's open folders, in
//! `$XDG_CONFIG_HOME/icloud-notes/window.json`. A file that is missing or
//! does not read gives the defaults: losing it costs a layout, nothing more.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::window;

pub struct Settings {
    pub width: i32,
    pub height: i32,
    /// The folder list's and the note list's widths (wide), and the tree's (narrow).
    pub folder_pane: i32,
    pub note_pane: i32,
    pub tree_pane: i32,
    pub zoom: f64,
    pub expanded: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            width: 1100,
            height: 700,
            folder_pane: window::FOLDER_PANE,
            note_pane: window::NOTE_PANE,
            tree_pane: window::TREE_PANE,
            zoom: 1.0,
            expanded: Vec::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("icloud-notes").join("window.json"))
}

impl Settings {
    pub fn load() -> Settings {
        let saved: Value = path()
            .and_then(|path| icloud_app::store::read_json(&path).ok().flatten())
            .unwrap_or_default();
        let d = Settings::default();
        let int = |key: &str, default: i32| {
            saved[key]
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .filter(|&n| n > 0)
                .unwrap_or(default)
        };
        Settings {
            width: int("width", d.width),
            height: int("height", d.height),
            folder_pane: int("folder_pane", d.folder_pane),
            note_pane: int("note_pane", d.note_pane),
            tree_pane: int("tree_pane", d.tree_pane),
            zoom: saved["zoom"].as_f64().unwrap_or(d.zoom),
            expanded: saved["expanded"]
                .as_array()
                .map(|folders| folders.iter().filter_map(|f| f.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        }
    }

    /// Written as the window closes; a failure only costs the next layout.
    pub fn save(&self) {
        let Some(path) = path() else { return };
        let saved = json!({
            "width": self.width,
            "height": self.height,
            "folder_pane": self.folder_pane,
            "note_pane": self.note_pane,
            "tree_pane": self.tree_pane,
            "zoom": self.zoom,
            "expanded": self.expanded,
        });
        let _ = icloud_app::store::write_json(&path, &saved);
    }
}
