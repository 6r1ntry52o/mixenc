//! Invariant 9: the toolchain's include folders (Visual Studio, Windows Kits) are not mirrored, so
//! clangd reads them as they are. Their headers should be UTF-8; the scan lists those that are not,
//! and counts the lines a UTF-8 reader would splice. The judgements are kept in a cache by size and
//! time, so a later scan reads only what changed.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::judge::{self, Encoding};
use crate::{convert, fsx, paths};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    pub mixenc: String,
    /// By normalized path.
    pub files: BTreeMap<String, Seen>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub size: u64,
    pub mtime: u64,
    pub judged: Encoding,
    #[serde(default)]
    pub traps: u32,
}

/// Whether clang reads such a header as it is meant (it reads UTF-8 only).
pub fn reads_right(judged: Encoding) -> bool {
    matches!(
        judged,
        Encoding::Ascii | Encoding::Utf8 | Encoding::Utf8Bom | Encoding::AmbiguousUtf8Cp932 | Encoding::Binary
    )
}

pub fn cache_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("toolchain.json")
}

pub fn load(state_dir: &Path) -> Cache {
    fs::read_to_string(cache_path(state_dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Cache>(&t).ok())
        .filter(|c| c.mixenc == env!("CARGO_PKG_VERSION"))
        .unwrap_or_default()
}

/// Judges the files under `dirs` (those not judged at this size and time yet) and saves the cache.
pub fn scan(dirs: &[String], state_dir: &Path) -> Result<()> {
    let mut cache = load(state_dir);
    let mut files = Vec::new();
    for dir in dirs {
        for rel in fsx::walk(Path::new(dir), &|_| false).unwrap_or_default() {
            files.push(paths::normalize(&format!("{dir}\\{rel}")));
        }
    }
    files.sort();
    files.dedup();
    let judged: Vec<(String, Option<Seen>)> = files
        .par_iter()
        .map(|file| {
            let Ok(meta) = fs::metadata(file) else { return (file.clone(), None) };
            let (size, mtime) = (meta.len(), fsx::mtime(&meta));
            if let Some(seen) = cache.files.get(file)
                && seen.size == size
                && seen.mtime == mtime
            {
                return (file.clone(), Some(seen.clone()));
            }
            let Ok(bytes) = fs::read(file) else { return (file.clone(), None) };
            let judged = judge::judge(&bytes);
            (file.clone(), Some(Seen { size, mtime, judged, traps: convert::traps(&bytes, judged) }))
        })
        .collect();
    for (file, seen) in judged {
        match seen {
            Some(seen) => cache.files.insert(file, seen),
            None => cache.files.remove(&file),
        };
    }
    cache.mixenc = env!("CARGO_PKG_VERSION").to_string();
    let text = serde_json::to_string(&cache)?;
    fsx::write_replace(&state_dir.join("tmp"), &cache_path(state_dir), text.as_bytes(), false)?;
    Ok(())
}

/// The headers under `dirs` that clang does not read right, from the cache.
pub fn findings<'a>(cache: &'a Cache, dirs: &[String]) -> Vec<(&'a str, &'a Seen)> {
    let dirs: Vec<String> = dirs.iter().map(|d| paths::normalize(d)).collect();
    cache
        .files
        .iter()
        .filter(|(file, seen)| !reads_right(seen.judged) && dirs.iter().any(|d| paths::is_under(file, d)))
        .map(|(file, seen)| (file.as_str(), seen))
        .collect()
}
