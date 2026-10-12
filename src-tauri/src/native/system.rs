//! Application settings, recent files, crash recovery and Windows shell helpers.

use crate::engine::session::{Dirs, write_file_atomically};
use crate::error::{BritError, ErrorKind, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ThemeMode {
    Light,
    Dark,
    #[default]
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PageLayoutMode {
    #[default]
    Continuous,
    Single,
    Facing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum OcrEngineChoice {
    /// Use the built-in Windows OCR engine if it is present.
    #[default]
    Windows,
    /// Never run OCR, even if an engine is available.
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AiProviderChoice {
    /// No AI provider configured. This is the default and the only state in which
    /// no document data could ever leave the machine.
    #[default]
    Disabled,
    /// A provider the user has configured themselves (OpenAI-compatible endpoint).
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    pub provider: AiProviderChoice,
    pub endpoint: String,
    pub model: String,
    /// True only while the user has explicitly switched AI on for a session.
    pub enabled: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            provider: AiProviderChoice::Disabled,
            endpoint: String::new(),
            model: String::new(),
            enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub theme: ThemeMode,
    pub page_layout: PageLayoutMode,
    pub default_zoom: f32,
    pub show_thumbnails: bool,
    pub show_search_highlights: bool,
    pub restore_session: bool,
    pub confirm_close_with_changes: bool,
    pub autosave_enabled: bool,
    pub autosave_minutes: u32,
    pub render_annotations: bool,
    pub smooth_scrolling: bool,
    pub show_page_borders: bool,
    pub recent_limit: usize,
    pub ocr_engine: OcrEngineChoice,
    pub ocr_language: String,
    pub ai: AiSettings,
    /// Set during installation when the user opted in to the .pdf association.
    pub pdf_file_association: bool,
    pub print_default_printer: Option<String>,
    pub print_copies: u32,
    pub print_color: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            page_layout: PageLayoutMode::Continuous,
            default_zoom: 1.0,
            show_thumbnails: true,
            show_search_highlights: true,
            restore_session: true,
            confirm_close_with_changes: true,
            autosave_enabled: true,
            autosave_minutes: 2,
            render_annotations: true,
            smooth_scrolling: true,
            show_page_borders: true,
            recent_limit: 12,
            ocr_engine: OcrEngineChoice::Windows,
            ocr_language: "en-US".into(),
            ai: AiSettings::default(),
            pdf_file_association: false,
            print_default_printer: None,
            print_copies: 1,
            print_color: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentEntry {
    pub path: String,
    pub file_name: String,
    pub folder: String,
    /// Unix milliseconds of the last successful open.
    pub last_opened: i64,
    pub page_count: Option<u32>,
    pub file_size: Option<u64>,
    pub thumbnail: Option<String>,
    /// Set by `get_recent_files` when the file has moved or been deleted.
    #[serde(default)]
    pub missing: bool,
}

/// Unsaved work that survived a crash or a forced quit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryEntry {
    pub doc_id: String,
    pub original_path: Option<String>,
    pub work_path: String,
    pub file_name: String,
    pub saved_at: i64,
    pub dirty: bool,
}

pub struct System {
    dirs: Dirs,
    settings: parking_lot::RwLock<Settings>,
    recents: parking_lot::RwLock<Vec<RecentEntry>>,
    recovery: parking_lot::RwLock<Vec<RecoveryEntry>>,
}

impl System {
    pub fn new(dirs: Dirs) -> Result<Self> {
        let settings = load_json(&dirs.settings_file()).unwrap_or_default();
        let recents = load_json::<Vec<RecentEntry>>(&dirs.recents_file()).unwrap_or_default();
        let recovery = load_json::<Vec<RecoveryEntry>>(&dirs.crash_file()).unwrap_or_default();
        let system = Self {
            dirs,
            settings: parking_lot::RwLock::new(settings),
            recents: parking_lot::RwLock::new(recents),
            recovery: parking_lot::RwLock::new(recovery),
        };
        system.prune_recents();
        Ok(system)
    }

    pub fn dirs(&self) -> &Dirs {
        &self.dirs
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    pub fn set_settings(&self, mut next: Settings) -> Result<Settings> {
        // Guard rails: keep the numbers inside ranges the UI can actually render.
        next.default_zoom = next.default_zoom.clamp(0.1, 10.0);
        next.recent_limit = next.recent_limit.clamp(0, 100);
        next.autosave_minutes = next.autosave_minutes.clamp(1, 120);
        *self.settings.write() = next.clone();
        save_json(&self.dirs.settings_file(), &next)?;
        Ok(next)
    }

    pub fn recents(&self) -> Vec<RecentEntry> {
        self.recents.read().clone()
    }

    /// Record a successful open, newest first, de-duplicated by path.
    pub fn touch_recent(&self, path: &PathBuf, page_count: Option<u32>, file_size: Option<u64>, thumbnail: Option<String>) -> Result<()> {
        let limit = self.settings().recent_limit;
        if limit == 0 {
            return Ok(());
        }
        let entry = RecentEntry {
            path: path.display().to_string(),
            file_name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Unknown.pdf".into()),
            folder: path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            last_opened: chrono::Utc::now().timestamp_millis(),
            page_count,
            file_size,
            thumbnail,
            missing: false,
        };
        {
            let mut list = self.recents.write();
            list.retain(|e| !e.path.eq_ignore_ascii_case(&entry.path));
            list.insert(0, entry);
            list.truncate(limit);
        }
        self.persist_recents()
    }

    pub fn remove_recent(&self, path: &str) -> Result<()> {
        self.recents.write().retain(|e| !e.path.eq_ignore_ascii_case(path));
        self.persist_recents()
    }

    pub fn clear_recents(&self) -> Result<()> {
        self.recents.write().clear();
        self.persist_recents()
    }

    fn persist_recents(&self) -> Result<()> {
        save_json(&self.dirs.recents_file(), &*self.recents.read())
    }

    /// Mark recents whose files have been moved or deleted, and drop the long-gone.
    pub fn prune_recents(&self) {
        let mut changed = false;
        {
            let mut list = self.recents.write();
            let before = list.len();
            list.retain_mut(|e| {
                let p = PathBuf::from(&e.path);
                if p.exists() {
                    e.missing = false;
                    true
                } else {
                    changed = true;
                    e.missing = true;
                    // Keep recently-used-but-missing entries for a short while so a
                    // file on a temporarily disconnected drive does not vanish.
                    let age_min = (chrono::Utc::now().timestamp_millis() - e.last_opened) / 60_000;
                    age_min < 120
                }
            });
            if list.len() != before {
                changed = true;
            }
            for e in list.iter_mut() {
                if !PathBuf::from(&e.path).exists() {
                    e.missing = true;
                }
            }
        }
        if changed {
            let _ = self.persist_recents();
        }
    }

    pub fn record_recovery(&self, entry: RecoveryEntry) {
        let mut list = self.recovery.write();
        if let Some(existing) = list.iter_mut().find(|e| e.doc_id == entry.doc_id) {
            *existing = entry;
        } else {
            list.push(entry);
        }
    }

    pub fn clear_recovery(&self, doc_id: &str) {
        self.recovery.write().retain(|e| e.doc_id != doc_id);
        let _ = save_json(&self.dirs.crash_file(), &*self.recovery.read());
    }

    pub fn recovery(&self) -> Vec<RecoveryEntry> {
        self.recovery.read().clone()
    }

    /// Remove recovery entries whose working file no longer exists.
    pub fn prune_recovery(&self) {
        self.recovery.write().retain(|e| PathBuf::from(&e.work_path).exists());
        let _ = save_json(&self.dirs.crash_file(), &*self.recovery.read());
    }
}

fn load_json<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_json<T: Serialize>(path: &std::path::Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| BritError::internal(format!("Could not serialise settings: {e}")))?;
    write_file_atomically(path, &bytes)
        .map_err(|e| BritError::new(ErrorKind::WriteFailed, format!("Could not save settings: {e}")))
}
