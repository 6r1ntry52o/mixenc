//! The doctor's short version: checks the mirrors, the judgements, the databases and the long paths
//! in a few seconds, changing nothing. Each finding is broken (needs doing), a notice (good to
//! know) or ok, with what to do. The report has a summary of numbers only and the details with
//! paths; both stay on this PC.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::config::{RepoPaths, Workspace};
use crate::db::{self, Mapper};
use crate::index::{Index, Problem};
use crate::judge::Encoding;
use crate::mirror::{self, Root};
use crate::sync::RepoState;
use crate::{fsx, git, paths, toolchain};

pub const FORMAT_VERSION: u32 = 1;
/// clangd names an index shard `<file name>.<16 hex digits>.idx`; NTFS allows 255 characters.
pub const CLANGD_SHARD_NAME_LIMIT: usize = 235;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Broken,
    Notice,
    Ok,
}

#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub severity: Severity,
    pub check: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    pub count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub broken: usize,
    pub notice: usize,
    pub ok: usize,
    pub files: usize,
    pub mirrored: usize,
    /// Lines the mirror reads right that a UTF-8 reader would splice.
    pub traps: u64,
    pub encodings: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub format_version: u32,
    pub workspace: String,
    pub summary: Summary,
    pub items: Vec<Item>,
}

struct Doctor<'a> {
    ws: &'a Workspace,
    repo: Option<String>,
    items: Vec<Item>,
    summary: Summary,
}

impl Doctor<'_> {
    fn add(&mut self, severity: Severity, check: &'static str, message: String, fix: Option<&str>, paths: Vec<String>) {
        let count = paths.len().max(1);
        self.items.push(Item {
            severity,
            check,
            repo: self.repo.clone(),
            message,
            fix: fix.map(str::to_string),
            count,
            paths,
        });
    }
}

pub fn run(ws: &Workspace) -> Report {
    let mut d = Doctor { ws, repo: None, items: Vec::new(), summary: Summary::default() };
    let mut long_paths = 0usize;
    for repo in ws.repos() {
        d.repo = Some(repo.name.clone());
        long_paths += check_repo(&mut d, &repo);
    }
    d.repo = None;
    check_overrides(&mut d);
    check_long_path_setting(&mut d, long_paths);

    d.items.sort_by_key(|i| i.severity);
    for item in &d.items {
        match item.severity {
            Severity::Broken => d.summary.broken += 1,
            Severity::Notice => d.summary.notice += 1,
            Severity::Ok => d.summary.ok += 1,
        }
    }
    Report { format_version: FORMAT_VERSION, workspace: paths::display(&ws.root), summary: d.summary, items: d.items }
}

/// Checks one repository; returns how many of its paths are longer than 260 characters.
fn check_repo(d: &mut Doctor, repo: &RepoPaths) -> usize {
    if !Index::exists(&repo.mirror) {
        d.add(Severity::Broken, "mirror", "not mirrored yet".into(), Some("mixenc sync"), Vec::new());
        return 0;
    }
    let state = RepoState::load(&repo.mirror);
    let roots = match &state {
        Some(s) if !s.roots.is_empty() => s.roots(),
        _ => vec![Root { source: repo.source.clone(), mirror: repo.mirror.clone() }],
    };
    let mut long_paths = 0;
    for (i, root) in roots.iter().enumerate() {
        long_paths += check_root(d, root, i == 0);
    }
    if let Some(state) = &state {
        check_db(d, repo, state, &roots);
        check_toolchain(d, state);
    }
    let longpaths = git::config_bool(&repo.source, "core.longpaths") == Some(true);
    if !longpaths {
        let severity = if long_paths > 0 { Severity::Broken } else { Severity::Notice };
        d.add(
            severity,
            "long-paths",
            format!("git's core.longpaths is off ({long_paths} paths over 260 characters)"),
            Some("git config --global core.longpaths true"),
            Vec::new(),
        );
    }
    long_paths
}

fn check_root(d: &mut Doctor, root: &Root, is_repo: bool) -> usize {
    let index = Index::load(&root.mirror, &root.source);
    let display = |rel: &str| d.ws.display_path(&paths::join_rel(&root.source, rel));
    let what = if is_repo { "the repository".to_string() } else { paths::display(&root.source) };

    // Invariant 8: originals changed, added or removed since the last sync.
    let listed =
        if is_repo { git::list_files(&root.source) } else { fsx::walk(&root.source, &|_| false).map_err(Into::into) };
    let mut stale = Vec::new();
    match listed {
        Ok(files) => {
            let known: BTreeSet<&str> = index.files.keys().map(String::as_str).collect();
            for rel in files.iter().filter(|f| !mirror::is_reserved(f) && !known.contains(f.as_str())) {
                if paths::join_rel(&root.source, rel).is_file() {
                    stale.push(display(rel));
                }
            }
        }
        Err(err) => d.add(Severity::Broken, "mirror", format!("cannot list {what}: {err:#}"), None, Vec::new()),
    }
    for (rel, state) in &index.files {
        match std::fs::metadata(paths::join_rel(&root.source, rel)) {
            Ok(meta) if meta.len() == state.size && fsx::mtime(&meta) == state.mtime => {}
            Ok(_) | Err(_) if state.problem.as_ref().is_some_and(Problem::retries) => {}
            _ => stale.push(display(rel)),
        }
    }
    if !stale.is_empty() {
        d.add(
            Severity::Broken,
            "mirror-stale",
            format!("{} files of {what} changed since the last sync (invariant 8)", stale.len()),
            Some("mixenc sync"),
            stale,
        );
    }

    // Invariant 2: the mirror is what the index says, read-only, and nothing else.
    match mirror::check_mirror(root, &index) {
        Ok(found) if !found.is_empty() => {
            let listed = found
                .iter()
                .map(|(rel, why)| format!("{} ({why})", d.ws.display_path(&paths::join_rel(&root.mirror, rel))))
                .collect();
            d.add(
                Severity::Broken,
                "mirror-integrity",
                format!("{} files in the mirror of {what} are not as written (invariant 2)", found.len()),
                Some("mixenc sync (writes them again)"),
                listed,
            );
        }
        Ok(_) => {}
        Err(err) => d.add(
            Severity::Broken,
            "mirror-integrity",
            format!("cannot read the mirror of {what}: {err:#}"),
            None,
            Vec::new(),
        ),
    }

    // Invariant 5 and the judgements.
    let mut undecided = Vec::new();
    let mut failed = Vec::new();
    let mut skipped = 0;
    let mut binary = 0;
    let mut notices: BTreeMap<Encoding, Vec<String>> = BTreeMap::new();
    let mut long_names = Vec::new();
    let mut long_paths = 0;
    for (rel, state) in &index.files {
        d.summary.files += 1;
        *d.summary.encodings.entry(state.judged.name().to_string()).or_default() += 1;
        d.summary.traps += u64::from(state.traps);
        if state.mirror.is_some() {
            d.summary.mirrored += 1;
        }
        match &state.problem {
            None => {}
            Some(Problem::Skipped) => skipped += 1,
            Some(Problem::Undecided) if state.judged == Encoding::Binary && !is_source(rel) => binary += 1,
            Some(Problem::Undecided) => undecided.push(format!("{} ({})", display(rel), state.judged)),
            Some(p @ (Problem::Violation(why) | Problem::Unreadable(why) | Problem::WriteFailed(why))) => {
                let kind = match p {
                    Problem::Violation(_) => "invariant 3/4",
                    Problem::Unreadable(_) => "cannot read",
                    _ => "cannot write",
                };
                failed.push(format!("{} ({kind}: {why})", display(rel)));
            }
        }
        if state.judged.is_notice() && state.mirror.is_some() {
            notices.entry(state.judged).or_default().push(display(rel));
        }
        let name = rel.rsplit('/').next().unwrap_or(rel);
        if is_source(rel) && name.encode_utf16().count() >= CLANGD_SHARD_NAME_LIMIT {
            long_names.push(display(rel));
        }
        let source_len = paths::join_rel(&root.source, rel).as_os_str().encode_wide_len();
        let mirror_len = paths::join_rel(&root.mirror, rel).as_os_str().encode_wide_len();
        if source_len.max(mirror_len) > 260 {
            long_paths += 1;
        }
    }
    if !undecided.is_empty() {
        d.add(
            Severity::Broken,
            "judgement",
            format!(
                "{} files of {what} need a person to decide their encoding; they are not mirrored (invariant 5)",
                undecided.len()
            ),
            Some("add them to \"overrides\" in mixenc.json with the encoding to read them as (or \"skip\")"),
            undecided,
        );
    }
    if !failed.is_empty() {
        d.add(
            Severity::Broken,
            "mirror-failed",
            format!("{} files of {what} could not be mirrored", failed.len()),
            Some("see the reason of each; files that could not be read or written are tried again at the next sync"),
            failed,
        );
    }
    for (encoding, files) in notices {
        let (message, fix) = notice_text(encoding);
        d.add(Severity::Notice, "judgement", format!("{} {message} in {what}", files.len()), Some(fix), files);
    }
    if !long_names.is_empty() {
        d.add(
            Severity::Notice,
            "long-paths",
            format!("{} files of {what} have names of {CLANGD_SHARD_NAME_LIMIT}+ characters: clangd cannot save their index and indexes them again at every start (answers stay right)", long_names.len()),
            Some("shorter names, if the CPU time matters"),
            long_names,
        );
    }
    if skipped + binary > 0 {
        d.add(
            Severity::Ok,
            "judgement",
            format!(
                "{} files of {what} not mirrored on purpose ({skipped} by the override table, {binary} binary)",
                skipped + binary
            ),
            None,
            Vec::new(),
        );
    }
    if index.files.is_empty() && !is_repo {
        d.add(Severity::Notice, "mirror", format!("{what} has no files"), None, Vec::new());
    }
    long_paths
}

fn notice_text(encoding: Encoding) -> (&'static str, &'static str) {
    match encoding {
        Encoding::Mixed => (
            "files mix UTF-8 and CP932 lines",
            "an editor reads the whole file in one encoding: edit them through mixenc, not VS Code (it opens them read-only)",
        ),
        Encoding::Broken => (
            "files have bytes that read in no encoding (mirrored as U+FFFD)",
            "look at the lines; the readable lines are mirrored",
        ),
        Encoding::AmbiguousUtf8Cp932 => (
            "files read both as UTF-8 and as CP932 (mirrored as UTF-8)",
            "if one is really CP932, add it to \"overrides\" with \"as\": \"cp932\"",
        ),
        Encoding::EucJp => ("files are EUC-JP", "MSVC reads them as CP932 unless the project says otherwise"),
        Encoding::Iso2022jp => ("files are ISO-2022-JP", "MSVC cannot read ISO-2022-JP"),
        _ => ("files are UTF-16 without a BOM", "MSVC needs a BOM to read UTF-16"),
    }
}

fn check_db(d: &mut Doctor, repo: &RepoPaths, state: &RepoState, roots: &[Root]) {
    let Some(db_state) = &state.db else {
        d.add(
            Severity::Broken,
            "db",
            "no compile_commands.json: clangd guesses the flags of every file".into(),
            Some("lazymsbuild sync (or \"db\" in mixenc.json), then mixenc sync"),
            Vec::new(),
        );
        return;
    };
    let source_mtime = std::fs::metadata(&db_state.source).map(|m| fsx::mtime(&m)).ok();
    if source_mtime != Some(db_state.source_mtime) {
        d.add(
            Severity::Broken,
            "db",
            "the database changed since the mirror database was written".into(),
            Some("mixenc sync"),
            vec![db_state.source.clone()],
        );
    }
    if !state.db_problems.is_empty() {
        d.add(
            Severity::Broken,
            "db",
            format!("{} problems writing the mirror database", state.db_problems.len()),
            Some("see each; then mixenc sync"),
            state.db_problems.clone(),
        );
    }
    if let Some(mirror_db) = &db_state.mirror {
        match db::read(Path::new(mirror_db)) {
            Ok(entries) => {
                let violations = db::check(&entries, &Mapper::new(roots));
                if violations.is_empty() {
                    let profile = db_state.profile.as_deref().map(|p| format!(" ({p})")).unwrap_or_default();
                    d.add(
                        Severity::Ok,
                        "db",
                        format!("mirror database{profile}: {} entries", entries.len()),
                        None,
                        Vec::new(),
                    );
                } else {
                    d.add(
                        Severity::Broken,
                        "db",
                        "the mirror database leads to originals (invariant 6)".into(),
                        Some("mixenc sync"),
                        violations,
                    );
                }
            }
            Err(err) => d.add(
                Severity::Broken,
                "db",
                format!("cannot read the mirror database: {err:#}"),
                Some("mixenc sync"),
                Vec::new(),
            ),
        }
    }
    if !state.missing_files.is_empty() {
        d.add(
            Severity::Notice,
            "db",
            format!("{} files the database names are not there", state.missing_files.len()),
            Some("lazymsbuild sync (or build once if they are generated)"),
            state.missing_files.iter().map(|f| d.ws.display_path(Path::new(f))).collect(),
        );
    }
    if !state.missing_dirs.is_empty() {
        d.add(
            Severity::Notice,
            "db",
            format!(
                "{} include folders do not exist (generated files such as .tlh appear after a build)",
                state.missing_dirs.len()
            ),
            Some("build once"),
            state.missing_dirs.iter().map(|f| d.ws.display_path(Path::new(f))).collect(),
        );
    }
    if repo.db.is_none()
        && let Some(problems) = lazymsbuild_check(&repo.source)
        && !problems.is_empty()
    {
        d.add(
            Severity::Notice,
            "db",
            format!("lazymsbuild check: {} problems (coverage, stale projects)", problems.len()),
            Some("lazymsbuild sync"),
            problems,
        );
    }
}

/// `lazymsbuild --json check`'s problems, when lazymsbuild is there and answers.
fn lazymsbuild_check(repo: &Path) -> Option<Vec<String>> {
    let output = Command::new("lazymsbuild").args(["--json", "check", "-C"]).arg(repo).output().ok()?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let problems = value.get("problems")?.as_array()?;
    Some(problems.iter().map(|p| p.as_str().map_or_else(|| p.to_string(), str::to_string)).collect())
}

fn check_toolchain(d: &mut Doctor, state: &RepoState) {
    if state.toolchain_dirs.is_empty() {
        return;
    }
    let cache = toolchain::load(&d.ws.state_dir());
    let findings = toolchain::findings(&cache, &state.toolchain_dirs);
    let (traps, others): (Vec<_>, Vec<_>) = findings.iter().partition(|(_, seen)| seen.traps > 0);
    if !traps.is_empty() {
        d.add(
            Severity::Broken,
            "toolchain",
            format!("{} toolchain headers have lines clangd splices (invariant 9)", traps.len()),
            Some("add their folder to \"external\" in mixenc.json so that it is mirrored"),
            traps.iter().map(|(f, s)| format!("{f} ({}, {} lines)", s.judged, s.traps)).collect(),
        );
    }
    if !others.is_empty() {
        d.add(
            Severity::Notice,
            "toolchain",
            format!("{} toolchain headers are not UTF-8 (clangd may misread their text)", others.len()),
            Some("add their folder to \"external\" in mixenc.json to mirror it"),
            others.iter().map(|(f, s)| format!("{f} ({})", s.judged)).collect(),
        );
    }
    if traps.is_empty() && others.is_empty() {
        d.add(
            Severity::Ok,
            "toolchain",
            format!("the toolchain's headers are UTF-8 ({} folders)", state.toolchain_dirs.len()),
            None,
            Vec::new(),
        );
    }
}

fn check_overrides(d: &mut Doctor) {
    let mut stale = Vec::new();
    let repos = d.ws.repos();
    for (written, abs, o) in d.ws.overrides() {
        let abs_s = abs.to_string_lossy().into_owned();
        let mut found = None;
        for repo in &repos {
            let roots = RepoState::load(&repo.mirror).map(|s| s.roots()).unwrap_or_default();
            for root in roots {
                if let Some(rest) = paths::rest_under(&abs_s, &root.source.to_string_lossy()) {
                    let index = Index::load(&root.mirror, &root.source);
                    found = index.files.get(&rest.replace('\\', "/")).map(|s| s.judged);
                }
            }
        }
        match (found, o.judged) {
            (None, _) => stale.push(format!("{written} (no such file)")),
            (Some(now), Some(then)) if now != then => stale.push(format!("{written} (judged {then} then, {now} now)")),
            _ => {}
        }
    }
    if !stale.is_empty() {
        d.add(
            Severity::Notice,
            "overrides",
            format!("{} overrides are out of date", stale.len()),
            Some("check them in mixenc.json"),
            stale,
        );
    }
}

fn check_long_path_setting(d: &mut Doctor, long_paths: usize) {
    match crate::win::long_paths_enabled() {
        Some(true) => d.add(
            Severity::Ok,
            "long-paths",
            format!("Windows long paths are on ({long_paths} paths over 260 characters)"),
            None,
            Vec::new(),
        ),
        _ => {
            let severity = if long_paths > 0 { Severity::Broken } else { Severity::Notice };
            d.add(
                severity,
                "long-paths",
                format!("Windows long paths are off (LongPathsEnabled; {long_paths} paths over 260 characters)"),
                Some("set HKLM\\SYSTEM\\CurrentControlSet\\Control\\FileSystem LongPathsEnabled to 1 (needs an administrator)"),
                Vec::new(),
            );
        }
    }
}

/// A C or C++ source or header (what clangd parses and indexes).
fn is_source(rel: &str) -> bool {
    let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(
        ext.as_str(),
        "c" | "cc"
            | "cpp"
            | "cxx"
            | "c++"
            | "h"
            | "hh"
            | "hpp"
            | "hxx"
            | "h++"
            | "inl"
            | "ipp"
            | "tlh"
            | "tli"
            | "inc"
            | "rc"
            | "rc2"
            | "idl"
    )
}

trait WideLen {
    fn encode_wide_len(&self) -> usize;
}

impl WideLen for std::ffi::OsStr {
    fn encode_wide_len(&self) -> usize {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().count()
    }
}

impl Report {
    /// The report as text; `limit` paths per item at most.
    pub fn to_text(&self, limit: usize) -> String {
        let mut out = format!("mixenc doctor: {}\n", self.workspace);
        let mut last = None;
        for item in &self.items {
            if last != Some(item.severity) {
                let title = match item.severity {
                    Severity::Broken => "Broken",
                    Severity::Notice => "Notice",
                    Severity::Ok => "OK",
                };
                out.push_str(&format!("\n{title}\n"));
                last = Some(item.severity);
            }
            let mark = match item.severity {
                Severity::Broken => "x",
                Severity::Notice => "!",
                Severity::Ok => "-",
            };
            let repo = item.repo.as_deref().map(|r| format!("{r}: ")).unwrap_or_default();
            out.push_str(&format!("  {mark} {repo}{}\n", item.message));
            if let Some(fix) = &item.fix {
                out.push_str(&format!("      fix: {fix}\n"));
            }
            for p in item.paths.iter().take(limit) {
                out.push_str(&format!("      {p}\n"));
            }
            if item.paths.len() > limit {
                out.push_str(&format!("      ... and {} more (--json lists all)\n", item.paths.len() - limit));
            }
        }
        let s = &self.summary;
        out.push_str(&format!(
            "\n{} broken, {} notices, {} ok; {} files, {} mirrored, {} lines a UTF-8 reader would splice\n",
            s.broken, s.notice, s.ok, s.files, s.mirrored, s.traps
        ));
        out
    }
}
