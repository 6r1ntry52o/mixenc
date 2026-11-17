//! Mirroring one root (a repository, or an include folder outside it) into its mirror folder.
//!
//! - Files whose size and time did not change are not read again.
//! - Files gone from the root are removed from the mirror first, so that a rename that changes only
//!   the case of a name does not remove the new file.
//! - Each mirror is checked (invariants 3 and 4) before it is written; a file that fails is listed
//!   and gets no mirror, so clangd never reads a wrong one.
//! - A file the judgement cannot decide waits for the override table; it is listed, not guessed.
//! - A file that could not be read or written is listed with the reason and tried again next time.
//! - Files in the mirror that no original stands for are removed (invariant 2).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::config::ReadAs;
use crate::index::{FileState, Index, MirrorState, Problem, STATE_DIR};
use crate::judge::{self, Encoding};
use crate::{convert, fsx, paths};

#[derive(Clone, Debug)]
pub struct Root {
    pub source: PathBuf,
    pub mirror: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub files: usize,
    pub written: usize,
    pub removed: usize,
    /// Mirror files changed or removed by hand while their original stayed the same, written again.
    pub repaired: usize,
    pub not_mirrored: Vec<(String, Problem)>,
}

/// Names at the root of a mirror that are not mirrored files: mixenc's state, the mirror database
/// and clangd's index. Originals with these names are left out.
pub fn is_reserved(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or_default();
    first.eq_ignore_ascii_case(STATE_DIR)
        || first.eq_ignore_ascii_case(".cache")
        || rel.eq_ignore_ascii_case("compile_commands.json")
}

/// How a file is read: as the override says, else as judged when the judgement decides it.
pub fn wanted_read_as(judged: Encoding, read_as: Option<ReadAs>) -> Option<Encoding> {
    match read_as {
        Some(ReadAs::Skip) => None,
        Some(ReadAs::Encoding(e)) => Some(e),
        None => judged.is_decided().then_some(judged),
    }
}

/// Mirrors `files` (relative to the root, with `/`) and removes what is no longer among them.
pub fn sync(root: &Root, files: Vec<String>, read_as: &(dyn Fn(&Path) -> Option<ReadAs> + Sync)) -> Result<Outcome> {
    let state_dir = root.mirror.join(STATE_DIR);
    fs::create_dir_all(&state_dir).with_context(|| format!("creating {}", paths::display(&state_dir)))?;
    let lock = fs::OpenOptions::new().create(true).truncate(false).write(true).open(state_dir.join("lock"))?;
    lock.lock().context("waiting for another mixenc on this mirror")?;
    let tmp = state_dir.join("tmp");
    let _ = fs::remove_dir_all(&tmp);

    let mut index = Index::load(&root.mirror, &root.source);
    let current = index.is_current();
    let files: Vec<String> = files.into_iter().filter(|f| !is_reserved(f)).collect();
    let mut outcome = Outcome { files: files.len(), ..Outcome::default() };

    let wanted: HashSet<&str> = files.iter().map(String::as_str).collect();
    let gone: Vec<String> = index.files.keys().filter(|k| !wanted.contains(k.as_str())).cloned().collect();
    for rel in gone {
        let target = paths::join_rel(&root.mirror, &rel);
        if fsx::remove(&target).is_ok() {
            if let Some(parent) = target.parent() {
                fsx::remove_empty_parents(parent, &root.mirror);
            }
            index.files.remove(&rel);
            outcome.removed += 1;
        }
    }

    let steps: Vec<(&String, Step)> =
        files.par_iter().map(|rel| (rel, step(root, rel, index.files.get(rel), current, read_as, &tmp))).collect();
    for (rel, step) in steps {
        match step {
            Step::Unchanged => {}
            Step::Gone => {
                index.files.remove(rel);
            }
            Step::State(state, kind) => {
                match kind {
                    Kind::Written => outcome.written += 1,
                    Kind::Repaired => outcome.repaired += 1,
                    Kind::Same => {}
                }
                index.files.insert(rel.clone(), *state);
            }
        }
    }
    for (rel, state) in &index.files {
        if let Some(problem) = &state.problem {
            outcome.not_mirrored.push((rel.clone(), problem.clone()));
        }
    }

    outcome.removed += remove_extras(root, &index)?;
    index.mixenc = env!("CARGO_PKG_VERSION").to_string();
    index.save(&root.mirror)?;
    let _ = fs::remove_dir_all(&tmp);
    Ok(outcome)
}

enum Step {
    Unchanged,
    /// The original is not a file any more.
    Gone,
    State(Box<FileState>, Kind),
}

enum Kind {
    /// The mirror on disk already matched.
    Same,
    Written,
    Repaired,
}

fn step(
    root: &Root,
    rel: &str,
    prev: Option<&FileState>,
    current: bool,
    read_as: &(dyn Fn(&Path) -> Option<ReadAs> + Sync),
    tmp: &Path,
) -> Step {
    let source = paths::join_rel(&root.source, rel);
    let target = paths::join_rel(&root.mirror, rel);
    let meta = match fs::metadata(&source) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Step::Gone,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Step::Gone,
        Err(err) => return unreadable(prev, &target, err),
    };
    let (size, mtime) = (meta.len(), fsx::mtime(&meta));
    let override_ = read_as(&source);

    if let Some(prev) = prev
        && current
        && prev.size == size
        && prev.mtime == mtime
        && !prev.problem.as_ref().is_some_and(Problem::retries)
        && wanted_read_as(prev.judged, override_) == prev.read_as
        && mirror_intact(&target, prev.mirror.as_ref())
    {
        return Step::Unchanged;
    }

    let bytes = match fs::read(&source) {
        Ok(bytes) => bytes,
        Err(err) => return unreadable(prev, &target, err),
    };
    let sha256 = fsx::sha256(&bytes);
    let same_original = prev.is_some_and(|p| p.sha256 == sha256);
    let judged = match prev {
        Some(p) if current && same_original => p.judged,
        _ => judge::judge(&bytes),
    };
    let mut state = FileState {
        size,
        mtime,
        sha256,
        judged,
        read_as: wanted_read_as(judged, override_),
        traps: 0,
        mirror: None,
        problem: None,
    };
    let Some(encoding) = state.read_as else {
        state.problem = Some(if override_ == Some(ReadAs::Skip) { Problem::Skipped } else { Problem::Undecided });
        return no_mirror(state, &target);
    };
    let Some(mirror) = convert::to_utf8(&bytes, encoding) else {
        state.problem = Some(Problem::Undecided);
        return no_mirror(state, &target);
    };
    if let Err(violation) = convert::check(&bytes, encoding, &mirror) {
        state.problem = Some(Problem::Violation(violation.to_string()));
        return no_mirror(state, &target);
    }
    state.traps = convert::traps(&bytes, encoding);
    let mirror_sha256 = fsx::sha256(&mirror);

    let prev_mirror = prev.and_then(|p| p.mirror.as_ref());
    if let Some(m) = prev_mirror
        && m.sha256 == mirror_sha256
        && mirror_intact(&target, Some(m))
    {
        state.mirror = Some(m.clone());
        return Step::State(Box::new(state), Kind::Same);
    }
    // The original is as it was, yet its mirror is not: someone changed or removed the mirror.
    let repaired = same_original && prev_mirror.is_some();
    match fsx::write_replace(tmp, &target, &mirror, true).and_then(|()| fs::metadata(&target)) {
        Ok(meta) => {
            state.mirror = Some(MirrorState { size: meta.len(), mtime: fsx::mtime(&meta), sha256: mirror_sha256 });
            Step::State(Box::new(state), if repaired { Kind::Repaired } else { Kind::Written })
        }
        Err(err) => {
            state.problem = Some(Problem::WriteFailed(err.to_string()));
            no_mirror(state, &target)
        }
    }
}

/// Whether the mirror file is the one the index recorded (or, with none recorded, absent).
pub fn mirror_intact(target: &Path, recorded: Option<&MirrorState>) -> bool {
    match (fs::metadata(target), recorded) {
        (Ok(meta), Some(m)) => meta.len() == m.size && fsx::mtime(&meta) == m.mtime && meta.permissions().readonly(),
        (Err(_), None) => true,
        _ => false,
    }
}

fn no_mirror(state: FileState, target: &Path) -> Step {
    let mut state = state;
    if let Err(err) = fsx::remove(target) {
        state.problem = Some(Problem::WriteFailed(format!("removing the old mirror: {err}")));
    }
    Step::State(Box::new(state), Kind::Same)
}

fn unreadable(prev: Option<&FileState>, target: &Path, err: std::io::Error) -> Step {
    let state = FileState {
        size: 0,
        mtime: 0,
        sha256: String::new(),
        judged: prev.map_or(Encoding::Binary, |p| p.judged),
        read_as: None,
        traps: 0,
        mirror: None,
        problem: Some(Problem::Unreadable(err.to_string())),
    };
    no_mirror(state, target)
}

/// Removes the files in the mirror that no original stands for. Returns how many.
fn remove_extras(root: &Root, index: &Index) -> Result<usize> {
    let keep: HashSet<String> =
        index.files.iter().filter(|(_, s)| s.mirror.is_some()).map(|(rel, _)| rel.to_lowercase()).collect();
    let extras: Vec<String> = fsx::walk(&root.mirror, &|rel| is_reserved(rel))?
        .into_iter()
        .filter(|rel| !keep.contains(&rel.to_lowercase()))
        .collect();
    for rel in &extras {
        let target = paths::join_rel(&root.mirror, rel);
        fsx::remove(&target).with_context(|| format!("removing {}", paths::display(&target)))?;
        if let Some(parent) = target.parent() {
            fsx::remove_empty_parents(parent, &root.mirror);
        }
    }
    Ok(extras.len())
}

/// The files of the mirror that are not as the index recorded them: changed or removed by hand,
/// not read-only, or with no original (for the doctor; nothing is changed).
pub fn check_mirror(root: &Root, index: &Index) -> Result<Vec<(String, &'static str)>> {
    let mut found = Vec::new();
    for (rel, state) in &index.files {
        let target = paths::join_rel(&root.mirror, rel);
        match (fs::metadata(&target), &state.mirror) {
            (Ok(meta), Some(m)) => {
                if meta.len() != m.size || fsx::mtime(&meta) != m.mtime {
                    found.push((rel.clone(), "changed by hand"));
                } else if !meta.permissions().readonly() {
                    found.push((rel.clone(), "not read-only"));
                }
            }
            (Err(_), Some(_)) => found.push((rel.clone(), "missing")),
            (Ok(_), None) => found.push((rel.clone(), "present though not mirrored")),
            (Err(_), None) => {}
        }
    }
    let known: HashSet<String> = index.files.keys().map(|rel| rel.to_lowercase()).collect();
    for rel in fsx::walk(&root.mirror, &|rel| is_reserved(rel))? {
        if !known.contains(&rel.to_lowercase()) {
            found.push((rel, "no original"));
        }
    }
    Ok(found)
}
