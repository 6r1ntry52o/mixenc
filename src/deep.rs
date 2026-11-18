//! The doctor's deep checks (`mixenc doctor --deep`), by hand and slow: every translation unit of
//! the mirror databases goes through `clangd --check`, and what it finds is held against the
//! baseline the first run took (`mirror/<repo>/.mixenc/deep-baseline.json`):
//!
//! - errors in a translation unit the baseline did not have: a file read wrong, or a generated file
//!   that is missing (MSVC builds the code, so clangd's errors should not grow);
//! - include files not found;
//! - headers of the repository no translation unit includes (clangd guesses their flags);
//! - how many translation units clangd's background index holds.
//!
//! The report stays on this PC, like the short one.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::config::{RepoPaths, Workspace};
use crate::doctor::{Item, Report, Severity, Summary};
use crate::index::{Index, STATE_DIR};
use crate::{db, entry, fsx, locate, paths};

const BASELINE_VERSION: u32 = 1;
/// Errors kept per translation unit in the baseline and the report.
const FIRST: usize = 3;

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// clangd processes at once (default: half the processors).
    pub jobs: Option<usize>,
    /// Write the baseline anew from this run.
    pub update_baseline: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitResult {
    pub errors: usize,
    /// The first errors, `line N: [code] message`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub first: Vec<String>,
    /// Include files not found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Baseline {
    format_version: u32,
    /// When it was taken (seconds since 1970).
    taken: u64,
    /// By the translation unit's path relative to the repository.
    units: BTreeMap<String, UnitResult>,
}

/// What `clangd --check` says about one file: its errors, the include files it did not find.
pub fn parse_check(stderr: &str) -> UnitResult {
    let mut r = UnitResult::default();
    for line in stderr.lines() {
        // E[12:00:00.000] [pp_file_not_found] Line 1: 'x.h' file not found
        let Some(rest) = line.strip_prefix("E[").and_then(|l| l.split_once("] [")).map(|(_, r)| r) else { continue };
        let Some((code, rest)) = rest.split_once("] ") else { continue };
        let Some(rest) = rest.strip_prefix("Line ") else { continue };
        let (number, message) = rest.split_once(": ").unwrap_or((rest, ""));
        r.errors += 1;
        if code == "pp_file_not_found"
            && let Some(name) = message.split('\'').nth(1)
        {
            r.missing.push(name.to_string());
        }
        if r.first.len() < FIRST {
            r.first.push(format!("line {number}: [{code}] {message}"));
        }
    }
    r
}

/// Runs the deep checks; `progress(done, total, file)` after each translation unit.
pub fn run(ws: &Workspace, opts: &Options, progress: &(dyn Fn(usize, usize, &str) + Sync)) -> Result<Report> {
    let clangd = entry::find_clangd(Some(ws))?;
    let jobs = opts.jobs.unwrap_or_else(|| std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).max(1)));
    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs).build()?;
    let mut items = Vec::new();
    let mut units = 0;
    for repo in ws.repos() {
        let db_path = repo.mirror.join("compile_commands.json");
        let entries = match db::read(&db_path) {
            Ok(entries) => entries,
            Err(err) => {
                items.push(item(
                    Severity::Broken,
                    "deep",
                    &repo,
                    format!("no mirror database: {err:#}"),
                    Some("mixenc sync"),
                    Vec::new(),
                ));
                continue;
            }
        };
        units += entries.len();
        let done = AtomicUsize::new(0);
        let total = entries.len();
        let results: Vec<(String, UnitResult)> = pool.install(|| {
            entries
                .par_iter()
                .map(|e| {
                    let mirror = PathBuf::from(paths::absolute(&e.directory, &e.file));
                    let rel = original_rel(ws, &repo, &mirror);
                    let result = check(&clangd, &mirror, &repo.mirror);
                    progress(done.fetch_add(1, Ordering::SeqCst) + 1, total, &rel);
                    (rel, result)
                })
                .collect()
        });
        items.extend(compare(ws, &repo, results, opts.update_baseline)?);
        items.extend(unread_headers(ws, &repo, &entries));
        items.push(index_state(&repo, &entries));
    }
    items.sort_by_key(|i| i.severity);
    let mut summary = Summary { units: Some(units), ..Summary::default() };
    for i in &items {
        match i.severity {
            Severity::Broken => summary.broken += 1,
            Severity::Notice => summary.notice += 1,
            Severity::Ok => summary.ok += 1,
        }
    }
    Ok(Report { format_version: crate::doctor::FORMAT_VERSION, workspace: paths::display(&ws.path), summary, items })
}

fn item(
    severity: Severity,
    check: &'static str,
    repo: &RepoPaths,
    message: String,
    fix: Option<&str>,
    paths: Vec<String>,
) -> Item {
    Item {
        severity,
        check,
        repo: Some(repo.name.clone()),
        message,
        fix: fix.map(str::to_string),
        count: paths.len().max(1),
        paths,
    }
}

/// A translation unit's original, relative to the repository (the mirror's path when it is none).
fn original_rel(ws: &Workspace, repo: &RepoPaths, mirror: &Path) -> String {
    let original = locate::find(mirror, Some(ws)).map_or_else(|| mirror.to_path_buf(), |p| p.original());
    paths::rest_under(&original.to_string_lossy(), &repo.source.to_string_lossy())
        .map_or_else(|| paths::forward(&original.to_string_lossy()), |r| r.replace('\\', "/"))
}

fn check(clangd: &Path, file: &Path, db_dir: &Path) -> UnitResult {
    let output = Command::new(clangd)
        .arg(format!("--check={}", paths::display(file)))
        .arg("--check-locations=false")
        .arg(format!("--compile-commands-dir={}", paths::display(db_dir)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(out) => parse_check(&String::from_utf8_lossy(&out.stderr)),
        Err(err) => UnitResult { errors: 1, first: vec![format!("clangd did not run: {err}")], missing: Vec::new() },
    }
}

/// Holds the results against the baseline (taking it on the first run).
fn compare(ws: &Workspace, repo: &RepoPaths, results: Vec<(String, UnitResult)>, update: bool) -> Result<Vec<Item>> {
    let path = repo.mirror.join(STATE_DIR).join("deep-baseline.json");
    let previous: Option<Baseline> = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(|b: &Baseline| b.format_version == BASELINE_VERSION);
    let current: BTreeMap<String, UnitResult> = results.into_iter().collect();
    let shown = |rel: &str, r: &UnitResult| {
        let abs = paths::join_rel(&repo.source, rel);
        format!("{} ({} errors: {})", ws.display_path(&abs), r.errors, r.first.first().map_or("", String::as_str))
    };
    let mut items = Vec::new();

    let mut missing: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (rel, r) in &current {
        for name in &r.missing {
            missing.entry(name.clone()).or_default().push(rel.clone());
        }
    }
    if !missing.is_empty() {
        let listed = missing
            .iter()
            .map(|(name, units)| {
                format!(
                    "{name} (from {}{})",
                    units[0],
                    if units.len() > 1 { format!(" and {} more", units.len() - 1) } else { String::new() }
                )
            })
            .collect();
        items.push(item(
            Severity::Notice,
            "deep-include",
            repo,
            format!("{} include files were not found (a generated file not built yet, a folder missing from the database)", missing.len()),
            Some("build once (generated files such as .tlh appear), or check the include folders in compile_commands.json"),
            listed,
        ));
    }

    match &previous {
        None => {
            let with_errors: Vec<String> =
                current.iter().filter(|(_, r)| r.errors > 0).map(|(rel, r)| shown(rel, r)).collect();
            items.push(item(
                if with_errors.is_empty() { Severity::Ok } else { Severity::Notice },
                "deep-baseline",
                repo,
                format!(
                    "baseline taken: {} translation units, {} with errors in clangd (later runs report only what grows)",
                    current.len(),
                    with_errors.len()
                ),
                (!with_errors.is_empty()).then_some("errors MSVC does not have are worth a look: a misread file, a flag clangd does not know"),
                with_errors,
            ));
        }
        Some(base) => {
            let grown: Vec<String> = current
                .iter()
                .filter(|(rel, r)| r.errors > base.units.get(*rel).map_or(0, |b| b.errors))
                .map(|(rel, r)| shown(rel, r))
                .collect();
            if grown.is_empty() {
                items.push(item(
                    Severity::Ok,
                    "deep-baseline",
                    repo,
                    format!("no translation unit has more errors than at the baseline ({} checked)", current.len()),
                    None,
                    Vec::new(),
                ));
            } else {
                items.push(item(
                    Severity::Broken,
                    "deep-baseline",
                    repo,
                    format!("{} translation units have more errors in clangd than at the baseline", grown.len()),
                    Some("a file read wrong (mixenc files), or a generated file missing; mixenc doctor --deep --update-baseline when the change is meant"),
                    grown,
                ));
            }
            let fewer =
                current.iter().filter(|(rel, r)| base.units.get(*rel).is_some_and(|b| r.errors < b.errors)).count();
            if fewer > 0 {
                items.push(item(
                    Severity::Ok,
                    "deep-baseline",
                    repo,
                    format!("{fewer} translation units have fewer errors than at the baseline"),
                    None,
                    Vec::new(),
                ));
            }
        }
    }
    if previous.is_none() || update {
        let taken = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let baseline = Baseline { format_version: BASELINE_VERSION, taken, units: current };
        fs::create_dir_all(path.parent().unwrap_or(&repo.mirror))?;
        fsx::write_replace(
            &repo.mirror.join(STATE_DIR).join("tmp"),
            &path,
            serde_json::to_string(&baseline)?.as_bytes(),
            false,
        )
        .with_context(|| format!("writing {}", paths::display(&path)))?;
    }
    Ok(items)
}

/// Headers of the repository that no translation unit includes, following `#include` in the mirror
/// (every `#include`, whatever `#if` it is in).
fn unread_headers(ws: &Workspace, repo: &RepoPaths, entries: &[db::Entry]) -> Vec<Item> {
    let mut seen: HashSet<String> = HashSet::new();
    for e in entries {
        let (dirs, forced) = db::includes_of(e);
        let mut stack: Vec<PathBuf> = forced.iter().map(|f| paths::canonical(Path::new(f))).collect();
        stack.push(paths::canonical(Path::new(&paths::absolute(&e.directory, &e.file))));
        while let Some(file) = stack.pop() {
            if !seen.insert(paths::key(&file.to_string_lossy())) {
                continue;
            }
            // Only files mixenc mirrors are followed (the toolchain's are not of interest here).
            if !paths::is_under(&file.to_string_lossy(), &ws.mirror_root().to_string_lossy()) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&file) else { continue };
            for (quoted, name) in includes(&text) {
                let here = file.parent().map(Path::to_path_buf);
                let found = here
                    .filter(|_| quoted)
                    .into_iter()
                    .chain(dirs.iter().map(PathBuf::from))
                    .map(|d| d.join(&name))
                    .find(|p| p.is_file());
                if let Some(p) = found {
                    stack.push(paths::canonical(&p));
                }
            }
        }
    }
    let index = Index::load(&repo.mirror, &repo.source);
    let unread: Vec<String> = index
        .files
        .iter()
        .filter(|(rel, s)| s.mirror.is_some() && is_header(rel))
        .filter(|(rel, _)| !seen.contains(&paths::key(&paths::join_rel(&repo.mirror, rel).to_string_lossy())))
        .map(|(rel, _)| ws.display_path(&paths::join_rel(&repo.source, rel)))
        .collect();
    if unread.is_empty() {
        return vec![item(
            Severity::Ok,
            "deep-headers",
            repo,
            "every header is included by a translation unit".into(),
            None,
            Vec::new(),
        )];
    }
    vec![item(
        Severity::Notice,
        "deep-headers",
        repo,
        format!(
            "{} headers are included by no translation unit of the database: clangd reads them with flags it guesses",
            unread.len()
        ),
        Some("expected for headers of projects not in the database; otherwise the database misses a project"),
        unread,
    )]
}

/// `#include` lines of a text: (quoted, the name as written).
fn includes(text: &str) -> Vec<(bool, String)> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix('#')?.trim_start().strip_prefix("include")?.trim_start();
            let (close, quoted) = match rest.chars().next()? {
                '"' => ('"', true),
                '<' => ('>', false),
                _ => return None,
            };
            let name = rest[1..].split(close).next()?;
            (!name.is_empty()).then(|| (quoted, name.replace('\\', "/")))
        })
        .collect()
}

fn is_header(rel: &str) -> bool {
    let ext = rel.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    rel.contains('.') && matches!(ext.as_str(), "h" | "hh" | "hpp" | "hxx" | "h++" | "inl" | "ipp")
}

/// How many translation units clangd's background index holds (a shard `<name>.<hash>.idx` each).
fn index_state(repo: &RepoPaths, entries: &[db::Entry]) -> Item {
    let dir = repo.mirror.join(".cache").join("clangd").join("index");
    let Ok(listing) = fs::read_dir(&dir) else {
        return item(
            Severity::Notice,
            "deep-index",
            repo,
            "clangd has no background index here yet (open the workspace once)".into(),
            None,
            Vec::new(),
        );
    };
    let shards: HashSet<String> = listing
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let stem = name.strip_suffix(".idx")?;
            stem.rsplit_once('.').map(|(file, _)| file.to_string())
        })
        .collect();
    let units: Vec<String> = entries
        .iter()
        .filter_map(|e| Path::new(&e.file).file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .collect();
    let indexed = units.iter().filter(|u| shards.contains(*u)).count();
    let severity = if indexed == units.len() { Severity::Ok } else { Severity::Notice };
    item(
        severity,
        "deep-index",
        repo,
        format!(
            "clangd's index holds {indexed} of the database's {} translation units (counted by file name)",
            units.len()
        ),
        (indexed < units.len()).then_some(
            "clangd may still be indexing; references and callers from the others are missing until it ends",
        ),
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_clangd_check() {
        let stderr = "I[08:42:57.800] Testing on source file x.cpp\n\
            E[08:42:57.819] [pp_file_not_found] Line 1: 'engine.h' file not found\n\
            E[08:42:57.831] IncludeCleaner: Failed to get an entry for resolved path '' from include \"engine.h\"\n\
            E[08:42:57.831] [undeclared_var_use] Line 4: use of undeclared identifier 'undefined_x'\n\
            I[08:42:57.900] All checks completed, 2 errors\n";
        let r = parse_check(stderr);
        assert_eq!(r.errors, 2);
        assert_eq!(r.missing, ["engine.h"]);
        assert_eq!(r.first[1], "line 4: [undeclared_var_use] use of undeclared identifier 'undefined_x'");
    }

    #[test]
    fn reads_include_lines() {
        let text = "#include \"a.h\"\n  #  include <sys\\b.h>\n// #include \"c.h\"\n#include MACRO\n#pragma once\n";
        assert_eq!(includes(text), [(true, "a.h".to_string()), (false, "sys/b.h".to_string())]);
        assert!(is_header("src/a.H") && is_header("x.inl") && !is_header("a.cpp") && !is_header("Makefile"));
    }
}
