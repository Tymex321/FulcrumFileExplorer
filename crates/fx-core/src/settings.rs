//! Persisted user settings and session state.
//!
//! Colour values deliberately live in the Tailwind config, not here — duplicating the
//! palette in Rust and CSS guarantees they drift. This type carries only the *choices* the
//! user makes; the UI owns what those choices look like.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::fsops::ListOptions;
use crate::query::Sort;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum Accent {
    #[default]
    Purple,
    Violet,
    Magenta,
    Indigo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum Density {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum ViewMode {
    #[default]
    Details,
    List,
    Grid,
    Tiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum PaneLayout {
    #[default]
    Single,
    /// Side-by-side dual pane.
    Vertical,
    /// Stacked panes.
    Horizontal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct Appearance {
    pub accent: Accent,
    pub density: Density,
    /// Translucent window background where the OS supports it. Off by default: the blur
    /// makes moving the window laggy, and it only shows through a transparent window.
    pub acrylic: bool,
    /// Honour the OS "reduce motion" setting; when false, animations are suppressed.
    pub animations: bool,
    pub font_size: u8,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            accent: Accent::default(),
            density: Density::default(),
            acrylic: false,
            animations: true,
            font_size: 13,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct IndexSettings {
    /// Whether to use the privileged helper for MFT indexing at all. When false the app
    /// stays on the walkdir backend permanently.
    pub instant_search: bool,
    /// Volume ids to index. Empty means all indexable volumes.
    pub volumes: Vec<String>,
    /// Paths excluded from indexing, as substring matches.
    pub exclusions: Vec<String>,
    /// Rescan on app start even when the helper already holds an index.
    pub rescan_on_start: bool,
}

impl Default for IndexSettings {
    fn default() -> Self {
        Self {
            instant_search: true,
            volumes: Vec::new(),
            // Noise that is never what anyone is searching for, and which otherwise
            // dominates results for common words.
            exclusions: vec![
                "\\$RECYCLE.BIN".into(),
                "\\System Volume Information".into(),
                "\\Windows\\WinSxS".into(),
                "\\node_modules".into(),
            ],
            rescan_on_start: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct BehaviorSettings {
    /// Single click opens, rather than double click.
    pub single_click_open: bool,
    pub confirm_delete: bool,
    /// Permanently delete instead of using the Recycle Bin. Off by default, obviously.
    pub skip_recycle_bin: bool,
    /// Restore tabs and panes from the previous session on launch.
    pub restore_session: bool,
    /// Look for a newer Fulcrum on startup and offer to install it. The check contacts
    /// GitHub; nothing is downloaded or installed without the user choosing to.
    pub check_for_updates: bool,
    pub default_view: ViewMode,
    pub list: ListOptions,
    pub sort: Sort,
}

impl Default for BehaviorSettings {
    fn default() -> Self {
        Self {
            single_click_open: false,
            confirm_delete: true,
            skip_recycle_bin: false,
            restore_session: true,
            check_for_updates: true,
            default_view: ViewMode::default(),
            list: ListOptions::default(),
            sort: Sort::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    pub index: IndexSettings,
    pub behavior: BehaviorSettings,
    /// Folders pinned to the sidebar.
    pub pinned: Vec<String>,
    /// Keybinding overrides as `command id -> chord`, e.g. `"pane.split" -> "Ctrl+\\"`.
    /// Defaults live in the UI's keymap; this only records what the user changed.
    pub keybindings: Vec<(String, String)>,
}

/// One open tab. Persisted so sessions survive a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TabState {
    pub path: String,
    pub view: ViewMode,
    pub sort: Sort,
    /// Back/forward history for this tab.
    pub history: Vec<String>,
    pub history_pos: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct SessionState {
    pub layout: PaneLayout,
    /// Tabs per pane. Length matches the active layout: 1 for `Single`, 2 otherwise.
    pub panes: Vec<Vec<TabState>>,
    pub active_pane: u32,
    pub active_tab: Vec<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_saved_before_the_update_check_existed_still_load_with_it_on() {
        // An older settings.json has no `check_for_updates`; it must not reset the rest.
        let old = r#"{"behavior":{"single_click_open":true,"restore_session":false}}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert!(s.behavior.single_click_open);
        assert!(!s.behavior.restore_session);
        assert!(s.behavior.check_for_updates);
    }

    #[test]
    fn the_update_check_can_be_turned_off_and_stays_off() {
        let mut s = Settings::default();
        s.behavior.check_for_updates = false;
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(!back.behavior.check_for_updates);
    }

    #[test]
    fn acrylic_is_off_by_default() {
        assert!(!Settings::default().appearance.acrylic);
    }
}
