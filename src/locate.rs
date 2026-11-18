//! Where a path is in a workspace: which mirrored root holds it (a repository, or an include folder
//! outside it), and whether the path is the original or its mirror. The analysis tools and the
//! Copilot hooks start from here.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{ReadAs, RepoPaths, Workspace};
use crate::index::FileState;
use crate::judge::{self, Encoding};
use crate::mirror::{self, Root};
use crate::sync::RepoState;
use crate::{doc, paths};

/// A mirrored root of a workspace.
#[derive(Clone, Debug)]
pub struct WsRoot {
    pub repo: RepoPaths,
    pub root: Root,
    /// An include folder outside the repositories.
    pub external: bool,
}

/// Every mirrored root of a workspace, the most specific first: each repository, and the include
/// folders its last sync mirrored.
pub fn roots(ws: &Workspace) -> Vec<WsRoot> {
    let mut out = Vec::new();
    for repo in ws.repos() {
        // The state of this repository's mirror (not of another original's, set there by mistake).
        let state = RepoState::load(&repo.mirror)
            .filter(|s| paths::key(&s.source) == paths::key(&repo.source.to_string_lossy()));
        match state {
            Some(state) => out.extend(state.roots.iter().map(|r| WsRoot {
                repo: repo.clone(),
                root: Root { source: PathBuf::from(&r.source), mirror: PathBuf::from(&r.mirror) },
                external: r.external,
            })),
            None => out.push(WsRoot {
                root: Root { source: repo.source.clone(), mirror: repo.mirror.clone() },
                repo,
                external: false,
            }),
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert(paths::key(&r.root.source.to_string_lossy())));
    out.sort_by_key(|r| std::cmp::Reverse(r.root.source.as_os_str().len()));
    out
}

/// A file (or folder) of a mirrored root.
#[derive(Clone, Debug)]
pub struct Place {
    pub ws: Workspace,
    pub at: WsRoot,
    /// Relative to the root, with `/`; empty for the root itself.
    pub rel: String,
    /// The path given was the mirror's.
    pub in_mirror: bool,
}

impl Place {
    pub fn original(&self) -> PathBuf {
        if self.rel.is_empty() { self.at.root.source.clone() } else { paths::join_rel(&self.at.root.source, &self.rel) }
    }

    pub fn mirror(&self) -> PathBuf {
        if self.rel.is_empty() { self.at.root.mirror.clone() } else { paths::join_rel(&self.at.root.mirror, &self.rel) }
    }

    /// What the mirror's index says about the file.
    pub fn state(&self) -> Option<FileState> {
        mirror::file_state(&self.at.root, &self.rel)
    }

    pub fn read_as(&self) -> Option<ReadAs> {
        self.ws.override_for(&self.original()).map(|o| o.read_as)
    }

    /// How the original reads now: the override, else its judgement (`None` when it is not there).
    pub fn encoding(&self) -> Option<Encoding> {
        let bytes = fs::read(self.original()).ok()?;
        let judged = judge::judge(&bytes);
        Some(match self.read_as() {
            Some(ReadAs::Encoding(e)) => e,
            _ => judged,
        })
    }

    /// Mirrors this one file now (as the entry does when an editor opens it).
    pub fn sync(&self) -> anyhow::Result<mirror::Outcome> {
        let ws = &self.ws;
        let read_as = |abs: &Path| ws.override_for(abs).map(|o| o.read_as);
        mirror::sync_files(&self.at.root, std::slice::from_ref(&self.rel), &read_as)
    }
}

/// Where `path` is: in `ws` if given, else in the workspace above it. `None` outside every root.
pub fn find(path: &Path, ws: Option<&Workspace>) -> Option<Place> {
    let abs = paths::canonical(&std::path::absolute(path).ok()?);
    let ws = match ws {
        Some(ws) => ws.clone(),
        None => Workspace::find(abs.parent().unwrap_or(&abs)).ok()?,
    };
    find_in(&abs, ws)
}

fn find_in(abs: &Path, ws: Workspace) -> Option<Place> {
    let s = abs.to_string_lossy();
    for at in roots(&ws) {
        for in_mirror in [true, false] {
            let base = if in_mirror { &at.root.mirror } else { &at.root.source };
            if let Some(rest) = paths::rest_under(&s, &base.to_string_lossy()) {
                let rel = rest.replace('\\', "/");
                if in_mirror && mirror::is_reserved(&rel) {
                    return None;
                }
                return Some(Place { ws, at, rel, in_mirror });
            }
        }
    }
    None
}

/// The text of a file as lines, read right: its mirror when it has one, else the original decoded
/// as judged (a toolchain header, a file of no workspace). `None` when it cannot be read.
pub fn lines(path: &Path, ws: Option<&Workspace>) -> Option<Vec<String>> {
    if let Some(place) = find(path, ws)
        && place.state().is_some_and(|s| s.mirror.is_some())
        && let Ok(bytes) = fs::read(place.mirror())
    {
        return Some(doc::parse(&bytes, Encoding::Utf8Bom)?.lines.into_iter().map(|l| l.text).collect());
    }
    let bytes = fs::read(path).ok()?;
    let judged = judge::judge(&bytes);
    let read = match judged {
        Encoding::AmbiguousCp932EucJp => Encoding::Cp932,
        Encoding::Binary => return None,
        e => e,
    };
    Some(doc::parse(&bytes, read)?.lines.into_iter().map(|l| l.text).collect())
}
