//! Reading candidate tracks from the local day cache built by [`crate::archive`].

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rayon::prelude::*;

use crate::archive::{self, IndexEntry};
use crate::geo::BBox;
use crate::trace::{self, Track};

/// Data root: `$OVERHEAD_DATA` (or the old `$DOPPLER_DATA`), else an
/// `overhead-data` folder next to the executable if there is one (portable
/// copies; an old `doppler-data` folder there too), else `~/overhead-data`
/// (`%USERPROFILE%` on Windows), or the old `~/doppler-data` while that one
/// exists and `~/overhead-data` does not, so an existing cache keeps being used.
pub fn data_root() -> PathBuf {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    let home = var("HOME").or_else(|| var("USERPROFILE")).unwrap_or_else(|| PathBuf::from("."));
    pick_data_root(var("OVERHEAD_DATA"), var("DOPPLER_DATA"), exe_dir.as_deref(), &home)
}

/// The choice of [`data_root`], from the two variables, the executable's
/// folder and the home folder.
fn pick_data_root(overhead: Option<PathBuf>, doppler: Option<PathBuf>, exe_dir: Option<&Path>, home: &Path) -> PathBuf {
    if let Some(d) = overhead.or(doppler) {
        return d;
    }
    if let Some(exe_dir) = exe_dir {
        for name in ["overhead-data", "doppler-data"] {
            let dir = exe_dir.join(name);
            if dir.is_dir() {
                return dir;
            }
        }
    }
    let new = home.join("overhead-data");
    let old = home.join("doppler-data");
    if !new.exists() && old.is_dir() { old } else { new }
}

/// Default cache root: `data_root()/adsb`.
pub fn default_root() -> PathBuf {
    data_root().join("adsb")
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, off)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, off)? {
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                off += n as u64;
            }
        }
    }
    Ok(())
}

pub struct Day {
    pub day: NaiveDate,
    dir: PathBuf,
    pub index: Vec<IndexEntry>,
}

impl Day {
    pub fn open(root: &Path, day: NaiveDate) -> Result<Self> {
        let dir = archive::day_dir(root, day);
        let f = File::open(dir.join("index.jsonl")).with_context(|| format!("{} not cached", day))?;
        let mut index = Vec::new();
        for line in BufReader::new(f).lines() {
            let line = line?;
            if !line.is_empty() {
                index.push(serde_json::from_str(&line)?);
            }
        }
        Ok(Self { day, dir, index })
    }

    /// Fetch the day if needed, then open it.
    pub fn ensure(root: &Path, day: NaiveDate, workers: usize) -> Result<Self> {
        archive::fetch_day(root, day, workers)?;
        Self::open(root, day)
    }

    /// Kept (candidate) tracks whose airborne extent touches `bbox` during `[t0, t1]`.
    pub fn tracks(&self, bbox: &BBox, t0: f64, t1: f64, max_min_alt: f64) -> Result<Vec<Track>> {
        let file = File::open(self.dir.join("kept.bin"))?;
        let sel: Vec<&IndexEntry> = self
            .index
            .iter()
            .filter(|e| e.kept.is_some())
            .filter(|e| e.bbox.is_some_and(|b| b.intersects(bbox)))
            .filter(|e| e.t1 >= t0 && e.t0 <= t1)
            .filter(|e| !(e.min_alt as f64 > max_min_alt))
            .collect();
        let tracks: Vec<Track> = sel
            .par_iter()
            .filter_map(|e| {
                let (off, len) = e.kept?;
                let mut buf = vec![0u8; len as usize];
                read_exact_at(&file, &mut buf, off).ok()?;
                trace::parse_gz(&buf).ok()
            })
            .collect();
        Ok(merge_same_hex(tracks))
    }
}

/// Merge traces of the same aircraft (several pods, or adjacent days).
pub fn merge_same_hex(tracks: Vec<Track>) -> Vec<Track> {
    let mut by: HashMap<String, Track> = HashMap::new();
    for t in tracks {
        match by.get_mut(&t.hex) {
            Some(a) => a.merge(t),
            None => {
                by.insert(t.hex.clone(), t);
            }
        }
    }
    let mut v: Vec<Track> = by.into_values().collect();
    v.sort_by(|a, b| a.hex.cmp(&b.hex));
    v
}

#[cfg(test)]
mod data_root_tests {
    use super::pick_data_root;
    use std::fs;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("overhead-data-root-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn variables_come_first() {
        let home = tmp("vars");
        let (a, b) = (PathBuf::from("/x/new"), PathBuf::from("/x/old"));
        assert_eq!(pick_data_root(Some(a.clone()), Some(b.clone()), None, &home), a);
        assert_eq!(pick_data_root(None, Some(b.clone()), None, &home), b);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn folder_next_to_the_executable() {
        let home = tmp("exe-home");
        let exe = tmp("exe-dir");
        // nothing next to the exe: the home folder
        assert_eq!(pick_data_root(None, None, Some(&exe), &home), home.join("overhead-data"));
        fs::create_dir(exe.join("doppler-data")).unwrap();
        assert_eq!(pick_data_root(None, None, Some(&exe), &home), exe.join("doppler-data"));
        fs::create_dir(exe.join("overhead-data")).unwrap();
        assert_eq!(pick_data_root(None, None, Some(&exe), &home), exe.join("overhead-data"));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&exe);
    }

    #[test]
    fn old_home_folder_while_the_new_one_is_missing() {
        let home = tmp("home");
        assert_eq!(pick_data_root(None, None, None, &home), home.join("overhead-data"));
        fs::create_dir(home.join("doppler-data")).unwrap();
        assert_eq!(pick_data_root(None, None, None, &home), home.join("doppler-data"));
        fs::create_dir(home.join("overhead-data")).unwrap();
        assert_eq!(pick_data_root(None, None, None, &home), home.join("overhead-data"));
        let _ = fs::remove_dir_all(&home);
    }
}
