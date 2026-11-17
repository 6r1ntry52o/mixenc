//! The encoding a file with no non-ASCII text yet gets when some is written into it (an ASCII file,
//! a new file): the one the files around it use.
//!
//! 1. The files to follow: those of the same project (the database's entries with its directory),
//!    else those in the same folder, else the whole repository; only those with non-ASCII text.
//! 2. The most common of `cp932`, `utf8-bom` and `utf8` among them; a tie is `cp932`.
//! 3. `utf8` without a BOM only when the project compiles with `/utf-8` (or
//!    `/source-charset:utf-8`): MSVC reads it as CP932 otherwise. When it is the most common
//!    without that, a person decides.
//! 4. Nothing to follow: `cp932`, MSVC's default.

use std::collections::HashMap;
use std::path::Path;

use crate::config::{RepoPaths, Workspace};
use crate::index::Index;
use crate::judge::Encoding;
use crate::sync::{self, RepoState};
use crate::{db, paths};

/// What to write non-ASCII text in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Write(Encoding),
    /// The files around are mostly UTF-8 without a BOM, which this project's MSVC reads as CP932.
    Ask,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub choice: Choice,
    /// Why, in a few words (what was counted where).
    pub why: String,
    /// The project compiles with `/utf-8` (or `/source-charset:utf-8`).
    pub utf8_flag: bool,
}

impl Pick {
    /// When there is nothing to follow (no workspace): CP932, MSVC's default.
    pub fn fallback() -> Pick {
        Pick {
            choice: Choice::Write(Encoding::Cp932),
            why: "nothing to follow: MSVC's default".into(),
            utf8_flag: false,
        }
    }

    /// Whether MSVC reads UTF-8 without a BOM right here, or a person has said to write it.
    pub fn allows_utf8(&self) -> bool {
        self.utf8_flag || matches!(self.choice, Choice::Write(Encoding::Utf8) | Choice::Ask)
    }
}

/// What the defaults of a repository are worked out from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Defaults {
    /// Each file's encoding (as mirrored), by its path relative to the repository, lower-case.
    files: HashMap<String, Encoding>,
    entries: Vec<EntryInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryInfo {
    /// The entry's directory relative to the repository, lower-case with `/` (empty at its root).
    pub dir: String,
    /// The source, the same way.
    pub file: String,
    pub utf8_flag: bool,
}

impl Defaults {
    pub fn new(files: HashMap<String, Encoding>, entries: Vec<EntryInfo>) -> Defaults {
        Defaults { files, entries }
    }

    /// The repository's index (from its last sync) and database.
    pub fn load(_ws: &Workspace, repo: &RepoPaths) -> Defaults {
        let index = Index::load(&repo.mirror, &repo.source);
        let files = index.files.iter().map(|(rel, s)| (rel.to_lowercase(), s.read_as.unwrap_or(s.judged))).collect();
        let db_file = RepoState::load(&repo.mirror)
            .and_then(|s| s.db)
            .map(|d| std::path::PathBuf::from(d.source))
            .filter(|p| p.is_file())
            .or_else(|| sync::locate_db(repo).0);
        let entries = db_file.and_then(|f| db::read(&f).ok()).unwrap_or_default();
        let root = repo.source.to_string_lossy();
        let rel =
            |abs: &str| paths::rest_under(&paths::long_form(abs), &root).map(|r| r.replace('\\', "/").to_lowercase());
        let entries = entries
            .iter()
            .filter_map(|e| {
                let directory = paths::long_form(&e.directory);
                Some(EntryInfo {
                    dir: rel(&directory)?,
                    file: rel(&paths::absolute(&directory, &e.file))?,
                    utf8_flag: db::compiles_as_utf8(e),
                })
            })
            .collect();
        Defaults { files, entries }
    }

    /// The default for the file at `rel` (relative to the repository, with `/`).
    pub fn for_file(&self, rel: &str) -> Pick {
        let rel = rel.to_lowercase();
        let folder = rel.rsplit_once('/').map_or("", |(dir, _)| dir).to_string();
        let project = self.entries.iter().find(|e| e.file == rel).map(|e| e.dir.clone()).or_else(|| {
            self.entries
                .iter()
                .filter(|e| e.dir.is_empty() || rel.starts_with(&format!("{}/", e.dir)))
                .max_by_key(|e| e.dir.len())
                .map(|e| e.dir.clone())
        });
        let project_entries: Vec<&EntryInfo> = match &project {
            Some(dir) => self.entries.iter().filter(|e| &e.dir == dir).collect(),
            None => self.entries.iter().collect(),
        };
        let utf8_flag = !project_entries.is_empty() && project_entries.iter().all(|e| e.utf8_flag);

        let levels: [(String, Vec<Encoding>); 3] = [
            (
                format!("project {}", project.as_deref().map_or("-", |d| if d.is_empty() { "." } else { d })),
                match &project {
                    Some(_) => project_entries.iter().filter_map(|e| self.files.get(&e.file).copied()).collect(),
                    None => Vec::new(),
                },
            ),
            (
                format!("folder {}", if folder.is_empty() { "." } else { &folder }),
                self.files
                    .iter()
                    .filter(|(f, _)| f.rsplit_once('/').map_or("", |(d, _)| d) == folder)
                    .map(|(_, e)| *e)
                    .collect(),
            ),
            ("the repository".to_string(), self.files.values().copied().collect()),
        ];
        for (place, encodings) in levels {
            let count = |e: Encoding| encodings.iter().filter(|&&x| x == e).count();
            let counts = [
                (Encoding::Cp932, count(Encoding::Cp932)),
                (Encoding::Utf8Bom, count(Encoding::Utf8Bom)),
                (Encoding::Utf8, count(Encoding::Utf8)),
            ];
            let total: usize = counts.iter().map(|(_, n)| n).sum();
            if total == 0 {
                continue;
            }
            let max = counts.iter().map(|(_, n)| *n).max().unwrap_or_default();
            let top: Vec<Encoding> = counts.iter().filter(|(_, n)| *n == max).map(|(e, _)| *e).collect();
            let winner = if top.len() > 1 { Encoding::Cp932 } else { top[0] };
            let why = format!(
                "{max} of the {total} files with non-ASCII text in {place} are {}{}",
                if top.len() > 1 { "tied; " } else { "" },
                if top.len() > 1 { "cp932 on a tie".to_string() } else { winner.name().to_string() }
            );
            let choice = match winner {
                Encoding::Utf8 if !utf8_flag => Choice::Ask,
                e => Choice::Write(e),
            };
            return Pick { choice, why, utf8_flag };
        }
        Pick { utf8_flag, ..Pick::fallback() }
    }
}

/// The default for a file of a workspace's repository, by its absolute path.
pub fn for_path(ws: &Workspace, repo: &RepoPaths, abs: &Path) -> Pick {
    match paths::rest_under(&abs.to_string_lossy(), &repo.source.to_string_lossy()) {
        Some(rel) => Defaults::load(ws, repo).for_file(&rel.replace('\\', "/")),
        None => Pick::fallback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults(files: &[(&str, Encoding)], entries: &[(&str, &str, bool)]) -> Defaults {
        Defaults::new(
            files.iter().map(|(f, e)| (f.to_string(), *e)).collect(),
            entries
                .iter()
                .map(|(d, f, u)| EntryInfo { dir: d.to_string(), file: f.to_string(), utf8_flag: *u })
                .collect(),
        )
    }

    #[test]
    fn follows_the_project_then_the_folder_then_the_repository() {
        let d = defaults(
            &[
                ("app/a.cpp", Encoding::Utf8Bom),
                ("app/b.cpp", Encoding::Utf8Bom),
                ("app/c.cpp", Encoding::Cp932),
                ("lib/x.cpp", Encoding::Cp932),
                ("lib/y.h", Encoding::Ascii),
                ("doc/n.txt", Encoding::Utf8),
            ],
            &[
                ("app", "app/a.cpp", false),
                ("app", "app/b.cpp", false),
                ("app", "app/c.cpp", false),
                ("lib", "lib/x.cpp", false),
            ],
        );
        assert_eq!(d.for_file("app/new.h").choice, Choice::Write(Encoding::Utf8Bom));
        assert_eq!(d.for_file("App/Sub/new.h").choice, Choice::Write(Encoding::Utf8Bom), "under the project's folder");
        assert_eq!(d.for_file("lib/y.h").choice, Choice::Write(Encoding::Cp932));
        // Not in a project: the folder (one UTF-8 file, without /utf-8 anywhere: ask).
        assert_eq!(d.for_file("doc/m.txt").choice, Choice::Ask);
        // Nothing in the folder: the repository (2 cp932, 2 utf8-bom, 1 utf8: a tie, cp932).
        let other = d.for_file("tools/t.py");
        assert_eq!(other.choice, Choice::Write(Encoding::Cp932));
        assert!(other.why.contains("tie"), "{}", other.why);
    }

    #[test]
    fn utf8_without_a_bom_needs_the_flag() {
        let files = [("a.cpp", Encoding::Utf8), ("b.cpp", Encoding::Utf8)];
        let with = defaults(&files, &[("", "a.cpp", true), ("", "b.cpp", true)]);
        assert_eq!(with.for_file("c.h").choice, Choice::Write(Encoding::Utf8));
        let without = defaults(&files, &[("", "a.cpp", true), ("", "b.cpp", false)]);
        assert_eq!(without.for_file("c.h").choice, Choice::Ask);
        assert!(!without.for_file("c.h").utf8_flag);
        assert_eq!(defaults(&[], &[]).for_file("c.h"), Pick::fallback());
    }
}
