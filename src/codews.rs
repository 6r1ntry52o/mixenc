//! The `.code-workspace` VS Code opens (`mixenc workspace`). The file is the person's: mixenc adds
//! its own items to it and leaves the rest (other folders, settings, comments) as it is. Its items:
//!
//! - the repositories as folders, when they are not there yet (other folders, such as documents,
//!   are the person's to add);
//! - `mixenc.configs`, the settings files this window uses (one per repository, or more);
//! - clangd through the entry (`clangd.path`, and `clangd.arguments` with `--mixenc-config` for
//!   each settings file: an editor has one clangd for all its folders);
//! - Microsoft's C/C++ IntelliSense off (two analyzers would disagree), no guessing of encodings;
//! - mixed and broken files read-only (an editor reads a file in one encoding, so a save would
//!   break their other lines); the extension keeps this list up to date;
//! - vscode-clangd and mixenc's extension among the recommended extensions.
//!
//! Adding the same settings file again changes nothing.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use serde_json::{Value, json};

use crate::config::Workspace;
use crate::entry::CONFIG_OPTION;
use crate::index::Index;
use crate::judge::Encoding;
use crate::{fsx, locate, paths};

/// The first line of the files earlier versions wrote whole; it no longer holds and goes.
pub const OLD_MARKER: &str = "// Written by `mixenc workspace` from mixenc.json; change that and run it again.";
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

/// What `add` did (or, with `dry_run`, would do).
#[derive(Debug)]
pub struct Added {
    pub path: PathBuf,
    /// One line per change; empty when the file already had everything.
    pub changes: Vec<String>,
    pub written: bool,
    /// How to start Copilot CLI with the window's other folders (documents, vault) at hand.
    pub copilot: Option<String>,
}

/// Adds the workspaces' items to the `.code-workspace` at `file` (made when it is not there).
pub fn add(file: &Path, workspaces: &[Workspace], exe: &Path, dry_run: bool) -> Result<Added> {
    if workspaces.is_empty() {
        bail!("no settings file to add");
    }
    let file = paths::canonical(&std::path::absolute(file)?);
    let dir = file.parent().context("a .code-workspace has a folder")?.to_path_buf();
    let base = dir.to_string_lossy().into_owned();
    let text = match fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}\n".to_string(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", paths::display(&file))),
    };
    let mut changes = Vec::new();
    let body = match text.strip_prefix(OLD_MARKER) {
        Some(rest) => {
            changes.push("took out the line that said mixenc writes the whole file".to_string());
            rest.trim_start_matches(['\r', '\n'])
        }
        None => text.as_str(),
    };
    let root = CstRootNode::parse(body, &ParseOptions::default())
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", paths::display(&file)))?;
    let top = root.object_value().with_context(|| format!("{} is not a JSON object", paths::display(&file)))?;
    // Paths in the file: relative to its folder with `/` when inside it, else absolute.
    let shown = |p: &Path| match paths::rest_under(&p.to_string_lossy(), &base) {
        Some(rest) if !rest.is_empty() => rest.replace('\\', "/"),
        _ => paths::forward(&p.to_string_lossy()),
    };
    let resolve = |s: &str| paths::key(&paths::absolute(&base, s));

    // Folders: the repositories, when not there.
    let folders = top.array_value_or_set("folders");
    let mut have: Vec<String> = folders
        .elements()
        .iter()
        .filter_map(|e| e.to_serde_value())
        .filter_map(|v| v["path"].as_str().map(resolve))
        .collect();
    for ws in workspaces {
        for repo in ws.repos() {
            let key = paths::key(&repo.source.to_string_lossy());
            if !have.contains(&key) {
                folders.append(input(&json!({ "name": repo.name, "path": shown(&repo.source) })));
                changes.push(format!("folders: added {}", paths::display(&repo.source)));
                have.push(key);
            }
        }
    }

    let settings = top.object_value_or_set("settings");
    // The settings files: those there and these, each once.
    let mut configs: Vec<String> = match settings.get("mixenc.configs").and_then(|p| p.value()) {
        Some(v) => v.to_serde_value().and_then(|v| v.as_array().cloned()).unwrap_or_default(),
        None => Vec::new(),
    }
    .iter()
    .filter_map(|v| v.as_str().map(str::to_string))
    .collect();
    for ws in workspaces {
        if !configs.iter().any(|c| resolve(c) == paths::key(&ws.path.to_string_lossy())) {
            configs.push(shown(&ws.path));
        }
    }
    set(&settings, "mixenc.configs", json!(configs), &mut changes);
    if let Some(old) = settings.get("mixenc.workspace") {
        old.remove();
        changes.push("settings: took out mixenc.workspace (mixenc.configs says it now)".to_string());
    }
    let exe = paths::forward(&exe.to_string_lossy());
    set(&settings, "mixenc.path", json!(exe), &mut changes);
    set(&settings, "clangd.path", json!(exe), &mut changes);
    // clangd's arguments: the person's kept, the settings files given to the entry.
    let current: Option<Vec<String>> = settings
        .get("clangd.arguments")
        .and_then(|p| p.value())
        .and_then(|v| v.to_serde_value())
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()));
    let mut args = vec!["clangd".to_string()];
    match &current {
        Some(current) => {
            let mut it = current.iter().skip_while(|a| *a == "clangd");
            while let Some(a) = it.next() {
                if a == CONFIG_OPTION {
                    it.next();
                } else if !a.starts_with(&format!("{CONFIG_OPTION}=")) {
                    args.push(a.clone());
                }
            }
        }
        None => args.push("--background-index".to_string()),
    }
    for c in &configs {
        args.push(CONFIG_OPTION.to_string());
        args.push(paths::forward(&paths::absolute(&base, c)));
    }
    set(&settings, "clangd.arguments", json!(args), &mut changes);
    set(&settings, "C_Cpp.intelliSenseEngine", json!("disabled"), &mut changes);
    set(&settings, "files.autoGuessEncoding", json!(false), &mut changes);

    // Read-only: mixenc's entries (literal paths under its roots) follow the judgements.
    let roots: Vec<String> =
        workspaces.iter().flat_map(locate::roots).map(|r| glob_literal(&r.root.source).to_lowercase() + "/").collect();
    let current = settings
        .get("files.readonlyInclude")
        .and_then(|p| p.value())
        .and_then(|v| v.to_serde_value())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let mut readonly = serde_json::Map::new();
    for (glob, on) in &current {
        if !roots.iter().any(|r| glob.to_lowercase().starts_with(r)) {
            readonly.insert(glob.clone(), on.clone());
        }
    }
    for ws in workspaces {
        for p in readonly_files(ws) {
            readonly.insert(glob_literal(&p), Value::Bool(true));
        }
    }
    if readonly != current {
        set(&settings, "files.readonlyInclude", Value::Object(readonly), &mut changes);
    }

    let recommendations = top.object_value_or_set("extensions").array_value_or_set("recommendations");
    let present: Vec<String> =
        recommendations.elements().iter().filter_map(|e| e.to_serde_value()?.as_str().map(str::to_lowercase)).collect();
    for r in RECOMMENDED {
        if !present.contains(&r.to_lowercase()) {
            recommendations.append(CstInputValue::String(r.to_string()));
            changes.push(format!("extensions: recommended {r}"));
        }
    }

    // The window's other folders go to Copilot CLI with --add-dir.
    let repos: Vec<String> =
        workspaces.iter().flat_map(Workspace::repos).map(|r| paths::key(&r.source.to_string_lossy())).collect();
    let others: Vec<String> = folders
        .elements()
        .iter()
        .filter_map(|e| e.to_serde_value())
        .filter_map(|v| v["path"].as_str().map(|s| paths::absolute(&base, s)))
        .filter(|p| !repos.contains(&paths::key(p)))
        .map(|p| format!("--add-dir \"{p}\""))
        .collect();
    let copilot = (!others.is_empty()).then(|| format!("copilot {}", others.join(" ")));

    let written = !dry_run && !changes.is_empty();
    if written {
        let mut out = root.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", paths::display(&dir)))?;
        fsx::write_replace(&dir, &file, out.as_bytes(), false)
            .with_context(|| format!("writing {}", paths::display(&file)))?;
    }
    Ok(Added { path: file, changes, written, copilot })
}

/// Sets a property when its value differs (and says so).
fn set(object: &CstObject, key: &str, value: Value, changes: &mut Vec<String>) {
    match object.get(key) {
        Some(prop) => {
            if prop.value().and_then(|v| v.to_serde_value()).as_ref() != Some(&value) {
                prop.set_value(input(&value));
                changes.push(format!("settings: {key} set"));
            }
        }
        None => {
            object.append(key, input(&value));
            changes.push(format!("settings: {key} added"));
        }
    }
}

fn input(v: &Value) -> CstInputValue {
    match v {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(a) => CstInputValue::Array(a.iter().map(input).collect()),
        Value::Object(o) => CstInputValue::Object(o.iter().map(|(k, v)| (k.clone(), input(v))).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_only_the_file() {
        assert_eq!(glob_literal(Path::new(r"C:\w\a[1]{x}.cpp")), "C:/w/a[[]1[]][{]x[}].cpp");
    }

    fn workspace(root: &Path, name: &str, repo: &str) -> Workspace {
        fs::create_dir_all(root.join(repo)).unwrap();
        let file = root.join("vault").join(format!("{name}.mixenc.json"));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, format!(r#"{{"mirror":"../mirror","repos":[{{"path":"../{repo}"}}]}}"#)).unwrap();
        Workspace::load_file(&file).unwrap()
    }

    #[test]
    fn adds_its_items_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = paths::canonical(tmp.path());
        let a = workspace(&root, "a", "repoA");
        let b = workspace(&root, "b", "repoB");
        let file = root.join("vault").join("dev.code-workspace");
        let mine = "{\n  // my window\n  \"folders\": [\n    { \"path\": \".\" }, // the vault\n  ],\n  \"settings\": {\n    \"editor.tabSize\": 2,\n    \"clangd.arguments\": [\"clangd\", \"--header-insertion=never\"],\n    \"files.readonlyInclude\": { \"C:/elsewhere/x.h\": true },\n  },\n}\n";
        fs::write(&file, mine).unwrap();
        let exe = Path::new(r"C:\Tools\mixenc.exe");

        let dry = add(&file, std::slice::from_ref(&a), exe, true).unwrap();
        assert!(!dry.written && !dry.changes.is_empty());
        assert_eq!(fs::read_to_string(&file).unwrap(), mine, "a dry run writes nothing");

        let first = add(&file, std::slice::from_ref(&a), exe, false).unwrap();
        assert!(first.written);
        let second = add(&file, &[a.clone(), b.clone()], exe, false).unwrap();
        assert!(second.changes.iter().any(|c| c.contains("repoB")), "{:?}", second.changes);
        let again = add(&file, &[b, a], exe, false).unwrap();
        assert!(again.changes.is_empty() && !again.written, "{:?}", again.changes);

        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("// my window") && text.contains("// the vault"), "{text}");
        let v: Value = serde_json::to_value(
            CstRootNode::parse(&text, &ParseOptions::default()).unwrap().to_serde_value().unwrap(),
        )
        .unwrap();
        let s = &v["settings"];
        assert_eq!(s["editor.tabSize"], 2);
        assert_eq!(s["mixenc.configs"], json!(["a.mixenc.json", "b.mixenc.json"]));
        let args: Vec<&str> = s["clangd.arguments"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        assert_eq!(&args[..2], &["clangd", "--header-insertion=never"]);
        assert_eq!(args.iter().filter(|a| **a == CONFIG_OPTION).count(), 2);
        assert!(args[3].ends_with("/vault/a.mixenc.json"), "{args:?}");
        assert_eq!(s["clangd.path"], "C:/Tools/mixenc.exe");
        assert_eq!(s["files.readonlyInclude"], json!({ "C:/elsewhere/x.h": true }));
        let folders: Vec<&str> = v["folders"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
        assert_eq!(folders.len(), 3);
        assert!(folders[1].ends_with("/repoA") && folders[2].ends_with("/repoB"), "{folders:?}");
        assert_eq!(v["extensions"]["recommendations"], json!(RECOMMENDED));
        assert!(again.copilot.unwrap().contains("--add-dir"));
    }

    #[test]
    fn a_file_mixenc_wrote_whole_loses_its_first_line() {
        let tmp = tempfile::tempdir().unwrap();
        let root = paths::canonical(tmp.path());
        let a = workspace(&root, "a", "repoA");
        let file = root.join("old.code-workspace");
        fs::write(&file, format!("{OLD_MARKER}\n{{\n  \"settings\": {{ \"mixenc.workspace\": \"C:/x\" }}\n}}\n"))
            .unwrap();
        add(&file, &[a], Path::new("mixenc"), false).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(!text.contains(OLD_MARKER) && !text.contains("mixenc.workspace"), "{text}");
    }
}
