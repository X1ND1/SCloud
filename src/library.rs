//! Local library: likes and listening history (works without a SoundCloud account).

use crate::api::Track;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub likes: Vec<Track>,
    #[serde(default)]
    pub history: Vec<Track>,
    /// tracks marked 👎 in the wave are never suggested again
    #[serde(default)]
    pub disliked: Vec<u64>,
    /// wave learning: artist → weight (+ listened through, − skipped / 👎)
    #[serde(default)]
    pub artist_feedback: HashMap<String, f32>,
    /// recent search queries (newest first)
    #[serde(default)]
    pub searches: Vec<String>,
    /// own playlists (and ones imported from SoundCloud)
    #[serde(default)]
    pub playlists: Vec<Playlist>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub tracks: Vec<Track>,
    /// SoundCloud playlist id, if imported from there
    #[serde(default)]
    pub sc_id: Option<u64>,
}

/// The main file lives in AppData.
fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("scloud").join("library.json"))
}

/// Backup copy in Documents\SCloud: survives uninstalling the app and clearing AppData,
/// and if Documents syncs with OneDrive it moves to another PC too.
pub fn backup_path() -> Option<PathBuf> {
    dirs::document_dir().map(|d| d.join("SCloud").join("library.json"))
}

fn modified(p: &PathBuf) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Library {
    /// Load the newer of the two files (main / backup).
    pub fn load() -> Self {
        let mut files: Vec<PathBuf> = [path(), backup_path()]
            .into_iter()
            .flatten()
            .filter(|p| p.exists())
            .collect();
        files.sort_by_key(|p| std::cmp::Reverse(modified(p)));
        files
            .iter()
            .find_map(|p| {
                std::fs::read_to_string(p)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
            })
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Ok(s) = serde_json::to_string(self) else {
            return;
        };
        for p in [path(), backup_path()].into_iter().flatten() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, &s);
        }
    }

    pub fn is_liked(&self, id: u64) -> bool {
        self.likes.iter().any(|t| t.id == id)
    }

    pub fn toggle_like(&mut self, t: &Track) {
        if let Some(i) = self.likes.iter().position(|x| x.id == t.id) {
            self.likes.remove(i);
        } else {
            self.likes.insert(0, t.clone());
        }
        self.save();
    }

    /// Adjust the wave weight of the track's artists.
    pub fn feedback(&mut self, t: &Track, delta: f32) {
        for a in crate::wave::track_artists(t) {
            let w = self.artist_feedback.entry(a).or_insert(0.0);
            *w = (*w + delta).clamp(-5.0, 5.0);
        }
        self.save();
    }

    pub fn dislike(&mut self, t: &Track) {
        if !self.disliked.contains(&t.id) {
            self.disliked.push(t.id);
        }
        self.likes.retain(|x| x.id != t.id);
        self.feedback(t, -1.5);
    }

    pub fn push_search(&mut self, q: &str) {
        let q = q.trim();
        if q.is_empty() {
            return;
        }
        let lower = q.to_lowercase();
        self.searches.retain(|x| x.to_lowercase() != lower);
        self.searches.insert(0, q.to_string());
        self.searches.truncate(20);
        self.save();
    }

    pub fn push_history(&mut self, t: &Track) {
        self.history.retain(|x| x.id != t.id);
        self.history.insert(0, t.clone());
        self.history.truncate(200);
        self.save();
    }

    // ---------- playlists ----------

    pub fn playlist(&self, id: u64) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.id == id)
    }

    pub fn create_playlist(&mut self, name: &str) -> u64 {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_millis() as u64)
            .max(self.playlists.iter().map(|p| p.id + 1).max().unwrap_or(1));
        let name = if name.trim().is_empty() {
            format!("{} {}", crate::i18n::t("Плейлист"), self.playlists.len() + 1)
        } else {
            name.trim().to_string()
        };
        self.playlists.insert(0, Playlist { id, name, tracks: Vec::new(), sc_id: None });
        self.save();
        id
    }

    /// true = added, false = was already in the playlist.
    pub fn add_to_playlist(&mut self, pid: u64, t: &Track) -> bool {
        let Some(p) = self.playlists.iter_mut().find(|p| p.id == pid) else {
            return false;
        };
        if p.tracks.iter().any(|x| x.id == t.id) {
            return false;
        }
        p.tracks.push(t.clone());
        self.save();
        true
    }

    pub fn remove_from_playlist(&mut self, pid: u64, track_id: u64) {
        if let Some(p) = self.playlists.iter_mut().find(|p| p.id == pid) {
            p.tracks.retain(|x| x.id != track_id);
        }
        self.save();
    }

    pub fn rename_playlist(&mut self, pid: u64, name: &str) {
        if let Some(p) = self.playlists.iter_mut().find(|p| p.id == pid) {
            if !name.trim().is_empty() {
                p.name = name.trim().to_string();
            }
        }
        self.save();
    }

    pub fn delete_playlist(&mut self, pid: u64) {
        self.playlists.retain(|p| p.id != pid);
        self.save();
    }

    /// Import from SoundCloud: update an already imported playlist or add a new one.
    pub fn import_playlist(&mut self, sc_id: u64, name: &str, tracks: Vec<Track>) {
        if let Some(p) = self.playlists.iter_mut().find(|p| p.sc_id == Some(sc_id)) {
            p.name = name.to_string();
            p.tracks = tracks;
        } else {
            let id = self.create_playlist(name);
            if let Some(p) = self.playlists.iter_mut().find(|p| p.id == id) {
                p.tracks = tracks;
                p.sc_id = Some(sc_id);
            }
        }
        self.save();
    }
}
