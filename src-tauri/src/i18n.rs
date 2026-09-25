//! UI language for backend messages and the user's display preferences.
//!
//! Messages are written in Chinese and English side by side with [`tr!`];
//! the chosen language is a process-wide flag set from the frontend.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn is_en() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn set_english(en: bool) {
    ENGLISH.store(en, Ordering::Relaxed);
}

/// `tr!("中文 {x}", "English {x}", args…)` formats the message in the
/// current language. Both literals take the same arguments.
#[macro_export]
macro_rules! tr {
    ($zh:literal, $en:literal $(, $a:expr)* $(,)?) => {
        if $crate::i18n::is_en() {
            format!($en $(, $a)*)
        } else {
            format!($zh $(, $a)*)
        }
    };
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Prefs {
    /// "zh", "en", or empty for "not chosen yet" (the UI then follows the OS).
    #[serde(default)]
    pub lang: String,
    /// "system", "light" or "dark".
    #[serde(default)]
    pub theme: String,
}

fn prefs_file() -> PathBuf {
    crate::store::config_dir().join("settings.json")
}

pub fn load_prefs() -> Prefs {
    fs::read_to_string(prefs_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_prefs(prefs: &Prefs) -> Result<(), String> {
    let path = prefs_file();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(prefs).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| e.to_string())
}
