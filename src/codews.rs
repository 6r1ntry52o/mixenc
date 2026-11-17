//! The `.code-workspace` VS Code opens for a workspace (`mixenc workspace`): the repositories and
//! their documents, clangd through the entry, Microsoft's C/C++ IntelliSense off (two analyzers
//! would disagree), mixed and broken files read-only (an editor reads a file in one encoding, so a
//! save would break their other lines), and the extensions to recommend. It is made from
//! mixenc.json; what is edited in it by hand is lost at the next run. The extension keeps its
//! read-only list up to date.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::config::Workspace;
use crate::index::Index;
use crate::judge::Encoding;
use crate::{fsx, locate, paths};

pub const MARKER: &str = "// Written by `mixenc workspace` from mixenc.json; change that and run it again.";
/// vscode-clangd and this project's extension.
pub const RECOMMENDED: [&str; 2] = ["llvm-vs-code-extensions.vscode-clangd", "6r1ntry52o.mixenc"];

/// The originals an editor must not save: mixed and broken files that are mirrored.
pub fn readonly_files(ws: &Workspace) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for at in locate::roots(ws) {
        for (rel, state) in Index::load(&at.root.mirror, &at.root.source).files {
            if matches!(state.judged, Encoding::Mixed | Encoding::Broken) && state.mirror.is_some() {
                out.push(paths::join_rel(&at.root.source, &rel));
            }
        }
    }
    out
}

/// A path as a glob that matches only itself (`/`, special characters in brackets).
pub fn glob_literal(p: &Path) -> String {
    paths::forward(&p.to_string_lossy())
        .chars()
        .map(|c| if "[]{}*?".contains(c) { format!("[{c}]") } else { c.to_string() })
        .collect()
}

/// A path for the file: relative to the workspace with `/` when inside it, else absolute.
fn folder_path(ws: &Workspace, p: &Path) -> String {
    match paths::rest_under(&p.to_string_lossy(), &ws.root.to_string_lossy()) {
        Some(rest) if !rest.is_empty() => rest.replace('\\', "/"),
        _ => paths::forward(&p.to_string_lossy()),
    }
}

/// The file's JSON (without the marker line).
pub fn content(ws: &Workspace, exe: &Path) -> Value {
    let exe = paths::forward(&exe.to_string_lossy());
    let mut folders = Vec::new();
    for repo in ws.repos() {
        folders.push(json!({ "name": repo.name, "path": folder_path(ws, &repo.source) }));
        if let Some(wiki) = &repo.wiki {
            let name = wiki.file_name().map_or_else(|| "wiki".into(), |n| n.to_string_lossy().into_owned());
            folders.push(json!({ "name": name, "path": folder_path(ws, wiki) }));
        }
    }
    let readonly: Map<String, Value> =
        readonly_files(ws).iter().map(|p| (glob_literal(p), Value::Bool(true))).collect();
    json!({
        "folders": folders,
        "settings": {
            "mixenc.workspace": paths::forward(&ws.root.to_string_lossy()),
            "mixenc.path": exe,
            "clangd.path": exe,
            "clangd.arguments": ["clangd", "--background-index"],
            "C_Cpp.intelliSenseEngine": "disabled",
            "files.autoGuessEncoding": false,
            "files.readonlyInclude": readonly,
        },
        "extensions": { "recommendations": RECOMMENDED },
    })
}

/// Writes `<name>.code-workspace` (default: the first repository's name) in the workspace. A file
/// of that name that mixenc did not write is left alone unless `force`.
pub fn write(ws: &Workspace, exe: &Path, name: Option<&str>, force: bool) -> Result<PathBuf> {
    let name = match name {
        Some(n) => n.trim_end_matches(".code-workspace").to_string(),
        None => ws.repos().first().map(|r| r.name.clone()).context("mixenc.json names no repository")?,
    };
    let path = ws.root.join(format!("{name}.code-workspace"));
    if let Ok(existing) = fs::read_to_string(&path)
        && !existing.starts_with(MARKER)
        && !force
    {
        bail!("{} exists and mixenc did not write it; --force replaces it", paths::display(&path));
    }
    let text = format!("{MARKER}\n{}\n", serde_json::to_string_pretty(&content(ws, exe))?);
    fsx::write_replace(&ws.root, &path, text.as_bytes(), false)
        .with_context(|| format!("writing {}", paths::display(&path)))?;
    Ok(path)
}

/// How to start Copilot CLI with the documents at hand (`--add-dir` for each wiki).
pub fn copilot_command(ws: &Workspace) -> Option<String> {
    let dirs: Vec<String> = ws
        .repos()
        .iter()
        .filter_map(|r| r.wiki.as_ref())
        .map(|w| format!("--add-dir \"{}\"", paths::display(w)))
        .collect();
    (!dirs.is_empty()).then(|| format!("copilot {}", dirs.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_only_the_file() {
        assert_eq!(glob_literal(Path::new(r"C:\w\a[1]{x}.cpp")), "C:/w/a[[]1[]][{]x[}].cpp");
    }

    #[test]
    fn the_file_holds_repositories_documents_and_the_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("repos/cloneA")).unwrap();
        fs::write(root.join("mixenc.json"), r#"{"repos":[{"path":"repos/cloneA","wiki":"repos/cloneA_Wiki"}]}"#)
            .unwrap();
        let ws = Workspace::load(root).unwrap();
        let v = content(&ws, Path::new(r"C:\Tools\mixenc.exe"));
        assert_eq!(v["folders"][0], json!({ "name": "cloneA", "path": "repos/cloneA" }));
        assert_eq!(v["folders"][1], json!({ "name": "cloneA_Wiki", "path": "repos/cloneA_Wiki" }));
        assert_eq!(v["settings"]["clangd.path"], "C:/Tools/mixenc.exe");
        assert_eq!(v["settings"]["clangd.arguments"], json!(["clangd", "--background-index"]));
        let path = write(&ws, Path::new(r"C:\Tools\mixenc.exe"), None, false).unwrap();
        assert!(path.ends_with("cloneA.code-workspace"));
        assert!(fs::read_to_string(&path).unwrap().starts_with(MARKER));
        // Written again over its own file; someone else's is kept.
        write(&ws, Path::new(r"C:\Tools\mixenc.exe"), None, false).unwrap();
        fs::write(root.join("mine.code-workspace"), "{}").unwrap();
        assert!(write(&ws, Path::new("x"), Some("mine"), false).is_err());
        assert!(copilot_command(&ws).unwrap().contains("--add-dir"));
    }
}
