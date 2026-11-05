//! Recent recordings: which files were opened, the day / time / region typed
//! for each and the answer found, so one can be opened again with a click.
//! Only these few values are kept (no audio, spectrogram or ADS-B), in
//! `history.json` in the settings folder, newest first.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Most recordings kept.
pub const MAX: usize = 12;

/// What was typed in the form for a recording (as typed).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FormValues {
    pub day: String,
    pub day_end: String,
    /// "unknown", "approx" or "exact".
    pub time_mode: String,
    pub start: String,
    pub hour_from: String,
    pub hour_to: String,
    pub utc: String,
    pub city: String,
    pub state: String,
    pub country: String,
}

impl FormValues {
    /// The region, finest part first ("Berlin, Germany").
    pub fn region(&self) -> Option<String> {
        let parts: Vec<&str> = [&self.city, &self.state, &self.country].into_iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// The answer of the last search.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    /// One reliable place; otherwise `places` possible ones.
    pub confident: bool,
    pub places: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    /// Last opened (Unix seconds).
    pub opened: i64,
    pub duration_s: Option<f64>,
    pub flybys: Option<usize>,
    pub form: FormValues,
    /// A location search ran; `result` is its answer (`None`: no place found).
    pub searched: bool,
    pub result: Option<Found>,
    /// The file was not there when last looked for (not stored).
    #[serde(skip)]
    pub missing: bool,
}

/// Recent recordings, newest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub entries: Vec<Entry>,
}

impl History {
    pub fn get(&self, path: &Path) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    pub fn get_mut(&mut self, path: &Path) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.path == path)
    }

    /// `path` was opened at `now`: it moves to the top (a new one is added,
    /// the oldest beyond `MAX` dropped).
    pub fn opened(&mut self, path: &Path, name: &str, now: i64) {
        let mut e = match self.entries.iter().position(|e| e.path == path) {
            Some(i) => self.entries.remove(i),
            None => Entry { path: path.to_path_buf(), ..Default::default() },
        };
        e.name = name.to_string();
        e.opened = now;
        e.missing = false;
        self.entries.insert(0, e);
        self.entries.truncate(MAX);
    }

    pub fn remove(&mut self, path: &Path) {
        self.entries.retain(|e| e.path != path);
    }
}

/// The same file however it was reached (relative path, `.`, `..`), so it
/// is one entry. Not `canonicalize`: on Windows that gives `\\?\C:\…` paths
/// and it needs the file to exist.
pub fn key(path: &Path) -> PathBuf {
    use std::path::Component;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn file() -> Option<PathBuf> {
    crate::update::config_dir().map(|d| d.join("history.json"))
}

pub fn load() -> History {
    let mut h: History = file().and_then(|p| fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    h.entries.retain(|e| !e.path.as_os_str().is_empty());
    h.entries.truncate(MAX);
    h
}

pub fn save(h: &History) {
    let Some(p) = file() else { return };
    let Ok(json) = serde_json::to_vec_pretty(h) else { return };
    let res = (|| -> std::io::Result<()> {
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &p)
    })();
    if let Err(e) = res {
        log::warn!("cannot save the history to {}: {e}", p.display());
    }
}

/// "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago", or the date.
pub fn ago(then: i64, now: i64) -> String {
    let d = (now - then).max(0);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", d / 60),
        3600..86_400 => format!("{} h ago", d / 3600),
        86_400..172_800 => "yesterday".into(),
        172_800..1_209_600 => format!("{} days ago", d / 86_400),
        _ => chrono::DateTime::from_timestamp(then, 0).map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opened_moves_to_top_keeps_values_and_caps() {
        let mut h = History::default();
        h.opened(Path::new("/a.wav"), "a.wav", 1);
        h.get_mut(Path::new("/a.wav")).unwrap().form.day = "2025-02-09".into();
        h.opened(Path::new("/b.wav"), "b.wav", 2);
        h.opened(Path::new("/a.wav"), "a.wav", 3);
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries[0].path, Path::new("/a.wav"));
        assert_eq!(h.entries[0].opened, 3);
        assert_eq!(h.entries[0].form.day, "2025-02-09");
        for i in 0..20 {
            h.opened(Path::new(&format!("/{i}.wav")), "x", 10 + i);
        }
        assert_eq!(h.entries.len(), MAX);
        assert_eq!(h.entries[0].path, Path::new("/19.wav"));
        h.remove(Path::new("/19.wav"));
        assert!(h.get(Path::new("/19.wav")).is_none());
    }

    #[test]
    fn round_trips_and_reads_old_or_partial_files() {
        let mut h = History::default();
        h.opened(Path::new("/r.mp3"), "r.mp3", 5);
        let e = h.get_mut(Path::new("/r.mp3")).unwrap();
        e.searched = true;
        e.result = Some(Found { lat: 52.5, lon: 13.4, radius_m: 1800.0, confident: true, places: 1 });
        e.flybys = Some(3);
        let back: History = serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap();
        assert_eq!(back, h);
        let partial: History = serde_json::from_str(r#"{"entries":[{"path":"/x.wav"}]}"#).unwrap();
        assert_eq!(partial.entries[0].form, FormValues::default());
        assert_eq!(partial.entries[0].result, None);
        assert!(!partial.entries[0].searched);
    }

    #[test]
    fn key_folds_dots() {
        let k = key(Path::new("/rec/a/../b/./x.wav"));
        assert_eq!(k, PathBuf::from("/rec/b/x.wav"));
    }

    #[test]
    fn region_and_ago() {
        let f = FormValues { city: " Berlin ".into(), country: "Germany".into(), ..Default::default() };
        assert_eq!(f.region().as_deref(), Some("Berlin, Germany"));
        assert_eq!(FormValues::default().region(), None);
        assert_eq!(ago(100, 110), "just now");
        assert_eq!(ago(0, 7200), "2 h ago");
        assert_eq!(ago(0, 100_000), "yesterday");
        assert_eq!(ago(0, 3 * 86_400), "3 days ago");
    }
}
