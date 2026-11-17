//! A sync of the workspace, repository by repository:
//! 1. read the repository's database (lazymsbuild's) and see which folders and files it names;
//! 2. mirror the repository (git's files, and the generated files the database names);
//! 3. mirror the include folders outside it (`mirror/_ext/<name>`);
//! 4. write the mirror database (checked by invariant 6) and `repo.json` (the roots and clangd's
//!    `--path-mappings`, for the entry and the doctor);
//! 5. judge the toolchain's headers (invariant 9).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{RepoPaths, Workspace};
use crate::db::{self, Mapper};
use crate::index::STATE_DIR;
use crate::mirror::{self, Outcome, Root};
use crate::{fsx, git, paths, toolchain};

pub const REPO_STATE_VERSION: u32 = 1;

/// What the last sync found about a repository, for the entry and the doctor.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RepoState {
    pub format_version: u32,
    pub source: String,
    pub mirror: String,
    /// The roots it mirrors: the repository first, then the include folders outside it.
    pub roots: Vec<RootState>,
    /// clangd's `--path-mappings` value.
    pub path_mappings: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<DbState>,
    #[serde(default)]
    pub toolchain_dirs: Vec<String>,
    #[serde(default)]
    pub missing_dirs: Vec<String>,
    #[serde(default)]
    pub missing_files: Vec<String>,
    /// What went wrong with the database (an unreadable response file, invariant 6, ...).
    #[serde(default)]
    pub db_problems: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RootState {
    pub source: String,
    pub mirror: String,
    pub external: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DbState {
    pub source: String,
    /// When the original database was read (its time, in nanoseconds since 1970).
    pub source_mtime: u64,
    /// The mirror database, when it was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub entries: usize,
}

impl RepoState {
    pub fn path(mirror: &Path) -> PathBuf {
        mirror.join(STATE_DIR).join("repo.json")
    }

    pub fn load(mirror: &Path) -> Option<RepoState> {
        let text = std::fs::read_to_string(RepoState::path(mirror)).ok()?;
        serde_json::from_str(&text).ok().filter(|s: &RepoState| s.format_version == REPO_STATE_VERSION)
    }

    pub fn roots(&self) -> Vec<Root> {
        self.roots.iter().map(|r| Root { source: PathBuf::from(&r.source), mirror: PathBuf::from(&r.mirror) }).collect()
    }
}

/// The result of syncing one repository.
#[derive(Debug)]
pub struct RepoSync {
    pub name: String,
    pub repo: Outcome,
    pub external: Vec<(Root, Outcome)>,
    pub state: RepoState,
}

/// Where the repository's database is, and lazymsbuild's profile when it says.
pub fn locate_db(repo: &RepoPaths) -> (Option<PathBuf>, Option<String>) {
    if let Some(db) = &repo.db {
        let file = if db.is_dir() { db.join("compile_commands.json") } else { db.clone() };
        return (Some(file), None);
    }
    let output = Command::new("lazymsbuild").args(["--json", "path", "-C"]).arg(&repo.source).output();
    if let Ok(output) = output
        && output.status.success()
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        && let Some(file) = value["db_file"].as_str()
    {
        return (Some(PathBuf::from(file)), value["profile"].as_str().map(str::to_string));
    }
    let file = repo.source.join("compile_commands.json");
    (file.is_file().then_some(file), None)
}

pub fn sync_repo(ws: &Workspace, repo: &RepoPaths) -> Result<RepoSync> {
    let mut state = RepoState {
        format_version: REPO_STATE_VERSION,
        source: paths::display(&repo.source),
        mirror: paths::display(&repo.mirror),
        ..RepoState::default()
    };

    let (db_file, profile) = locate_db(repo);
    let entries = match &db_file {
        Some(file) if file.is_file() => match db::read(file) {
            Ok(entries) => entries,
            Err(err) => {
                state.db_problems.push(format!("{err:#}"));
                Vec::new()
            }
        },
        _ => Vec::new(),
    };
    let analysis = db::analyze(&entries, &repo.source);

    let mut external_dirs = analysis.external_dirs.clone();
    external_dirs.extend(ws.config.external.iter().map(|e| ws.resolve(e).to_string_lossy().into_owned()));
    let (existing, missing): (Vec<String>, Vec<String>) =
        db::outermost(external_dirs).into_iter().partition(|d| Path::new(d).is_dir());
    let external_roots: Vec<Root> = existing
        .into_iter()
        .map(|d| Root {
            mirror: ws.external_root().join(paths::short_name(&d)),
            source: paths::canonical(Path::new(&d)),
        })
        .collect();

    let read_as = |abs: &Path| ws.override_for(abs).map(|o| o.read_as);
    let mut files: BTreeSet<String> = git::list_files(&repo.source)?.into_iter().collect();
    files.extend(analysis.repo_files.iter().cloned());
    let repo_root = Root { source: repo.source.clone(), mirror: repo.mirror.clone() };
    let repo_outcome = mirror::sync(&repo_root, files.into_iter().collect(), &read_as)?;

    let mut external = Vec::new();
    for root in external_roots {
        let files =
            fsx::walk(&root.source, &|_| false).with_context(|| format!("listing {}", paths::display(&root.source)))?;
        let outcome = mirror::sync(&root, files, &read_as)?;
        external.push((root, outcome));
    }

    let mut roots = vec![repo_root.clone()];
    roots.extend(external.iter().map(|(r, _)| r.clone()));
    let mapper = Mapper::new(&roots);
    state.roots = roots
        .iter()
        .enumerate()
        .map(|(i, r)| RootState {
            source: paths::display(&r.source),
            mirror: paths::display(&r.mirror),
            external: i > 0,
        })
        .collect();
    state.path_mappings = mapper.path_mappings();
    state.toolchain_dirs = analysis.toolchain_dirs.clone();
    state.missing_dirs = analysis.missing_dirs.clone();
    state.missing_dirs.extend(missing);
    state.missing_files = analysis.missing_files.clone();

    if let Some(file) = &db_file
        && file.is_file()
    {
        let (mut mirror_entries, problems) = db::rewrite(&entries, &mapper);
        // The database may spell a path in another case than the disk; clangd finds an entry by the
        // path the editor's (disk-cased) path maps to.
        for e in &mut mirror_entries {
            let file = paths::absolute(&e.directory, &e.file);
            if Path::new(&file).is_file() && paths::is_absolute(&e.file) {
                e.file = paths::display(&paths::canonical(Path::new(&file)));
            }
            if Path::new(&e.directory).is_dir() {
                e.directory = paths::display(&paths::canonical(Path::new(&e.directory)));
            }
        }
        state.db_problems.extend(problems);
        let violations = db::check(&mirror_entries, &mapper);
        let mirror_db = repo.mirror.join("compile_commands.json");
        let written = if violations.is_empty() {
            let text = serde_json::to_string_pretty(&mirror_entries)? + "\n";
            fsx::write_replace(&repo.mirror.join(STATE_DIR).join("tmp"), &mirror_db, text.as_bytes(), false)
                .with_context(|| format!("writing {}", paths::display(&mirror_db)))?;
            Some(paths::display(&mirror_db))
        } else {
            state.db_problems.extend(violations.into_iter().map(|v| format!("invariant 6: {v}")));
            None
        };
        state.db = Some(DbState {
            source: paths::display(file),
            source_mtime: std::fs::metadata(file).map(|m| fsx::mtime(&m)).unwrap_or_default(),
            mirror: written,
            profile,
            entries: entries.len(),
        });
    }

    let text = serde_json::to_string_pretty(&state)? + "\n";
    fsx::write_replace(
        &repo.mirror.join(STATE_DIR).join("tmp"),
        &RepoState::path(&repo.mirror),
        text.as_bytes(),
        false,
    )?;
    toolchain::scan(&state.toolchain_dirs, &ws.state_dir())?;

    Ok(RepoSync { name: repo.name.clone(), repo: repo_outcome, external, state })
}

/// Syncs every repository of the workspace (or the one named).
pub fn sync(ws: &Workspace, only: Option<&str>) -> Result<Vec<RepoSync>> {
    let repos: Vec<RepoPaths> =
        ws.repos().into_iter().filter(|r| only.is_none_or(|n| r.name.eq_ignore_ascii_case(n))).collect();
    if repos.is_empty() {
        anyhow::bail!("no repository {} in {}", only.unwrap_or(""), paths::display(&ws.file()));
    }
    repos.iter().map(|r| sync_repo(ws, r).with_context(|| format!("syncing {}", r.name))).collect()
}
