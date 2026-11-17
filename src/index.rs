//! A mirror's index: for every file of its root, what the original was (size, time, hash), its
//! judgement, how it was read, and what was written. The next sync reads only the files whose size
//! or time changed, and the doctor checks the mirror against it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::judge::Encoding;
use crate::{fsx, paths};

/// mixenc's folder inside a mirror (and inside `mirror/` for the workspace's own state).
pub const STATE_DIR: &str = ".mixenc";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub format_version: u32,
    /// The mixenc that wrote it; another version reads every file again.
    pub mixenc: String,
    /// The root it mirrors.
    pub source: String,
    pub files: BTreeMap<String, FileState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    pub size: u64,
    pub mtime: u64,
    pub sha256: String,
    pub judged: Encoding,
    /// How the mirror was read (the judgement or the override); absent when not mirrored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_as: Option<Encoding>,
    /// Lines a UTF-8 reader would splice with the next one (a CP932 character ending in `0x5C` at
    /// the end of the line). The mirror reads them right.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub traps: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<MirrorState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<Problem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorState {
    pub size: u64,
    pub mtime: u64,
    pub sha256: String,
}

/// Why a file has no mirror.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "detail")]
pub enum Problem {
    /// The judgement needs a person (ambiguous between CP932 and EUC-JP, binary).
    Undecided,
    /// The override table says to leave it out.
    Skipped,
    /// The converted text broke invariant 3 or 4; the detail says how.
    Violation(String),
    /// The original could not be read (in use, access denied, ...); tried again at the next sync.
    Unreadable(String),
    /// The mirror could not be written; tried again at the next sync.
    WriteFailed(String),
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Problem {
    /// Whether the next sync tries the file again even if it did not change.
    pub fn retries(&self) -> bool {
        matches!(self, Problem::Unreadable(_) | Problem::WriteFailed(_))
    }
}

impl Index {
    pub fn path(mirror: &Path) -> PathBuf {
        mirror.join(STATE_DIR).join("index.json")
    }

    /// The index of a mirror, or an empty one when there is none or it is not for `source`.
    pub fn load(mirror: &Path, source: &Path) -> Index {
        let empty = || Index {
            format_version: FORMAT_VERSION,
            mixenc: env!("CARGO_PKG_VERSION").to_string(),
            source: paths::display(source),
            files: BTreeMap::new(),
        };
        let Ok(text) = fs::read_to_string(Index::path(mirror)) else { return empty() };
        match serde_json::from_str::<Index>(&text) {
            Ok(index)
                if index.format_version == FORMAT_VERSION
                    && paths::key(&index.source) == paths::key(&source.to_string_lossy()) =>
            {
                index
            }
            _ => empty(),
        }
    }

    /// Whether the mirror has an index at all.
    pub fn exists(mirror: &Path) -> bool {
        Index::path(mirror).is_file()
    }

    pub fn save(&self, mirror: &Path) -> Result<()> {
        let path = Index::path(mirror);
        let text = serde_json::to_string(self)?;
        fsx::write_replace(&mirror.join(STATE_DIR).join("tmp"), &path, text.as_bytes(), false)
            .with_context(|| format!("writing {}", paths::display(&path)))
    }

    /// Whether this index was written by this mixenc (else every file is read again).
    pub fn is_current(&self) -> bool {
        self.mixenc == env!("CARGO_PKG_VERSION")
    }
}
