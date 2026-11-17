//! Copilot's gate: hooks for VS Code's Copilot and Copilot CLI, which read the same hook files
//! (`~/.copilot/hooks/`). `mixenc copilot hook <event>` reads the event (JSON) on stdin and answers
//! on stdout:
//!
//! - `preToolUse`: a read of a file that is not UTF-8 goes to its mirror (`modifiedArgs`); a
//!   built-in write to such a file, or to a mirror, is denied with the way to the front gate, and
//!   so is non-ASCII text written where MSVC would read it as another encoding;
//! - `postToolUse`: grep's answer gets the matches in files that are not UTF-8, searched in their
//!   mirrors and named by their originals; after a write or a shell command, the mirror is synced;
//! - `sessionStart`: how to read and write here, and the doctor's short report, as context.
//!
//! Outside a mixenc workspace the hook says nothing. A write it cannot check is denied; anything
//! else it cannot handle goes through (the commit gate is the last line).
//!
//! `mixenc copilot install` puts the hooks, the analysis agent, the MCP server and the LSP entry
//! under `~/.copilot/` (`COPILOT_HOME`); `uninstall` takes them out again.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::config::{ReadAs, Workspace};
use crate::defaults::{Choice, Defaults};
use crate::judge::{self, Encoding};
use crate::locate::{self, Place};
use crate::search::{self, Matcher, Options};
use crate::{doctor, fsx, paths, sync};

/// The analysis agent's file, as installed.
pub const AGENT: &str = include_str!("../assets/mixenc-analyst.agent.md");
pub const AGENT_FILE: &str = "mixenc-analyst.agent.md";
/// The text to put in an orchestrator's instructions, so that it calls the analysis agent.
pub const INTRO: &str = include_str!("../assets/intro.md");
pub const INTRO_JA: &str = include_str!("../assets/intro.ja.md");
pub const HOOKS_FILE: &str = "mixenc.json";
const RECORD_FILE: &str = "mixenc-install.json";

/// The events the hooks file sends to mixenc, with their time limits (seconds).
const EVENTS: [(&str, u64); 3] = [("sessionStart", 20), ("preToolUse", 15), ("postToolUse", 60)];

// ---------------------------------------------------------------------------------------------
// The hook

/// One tool event, in either form Copilot sends (camelCase, or the snake_case of `PreToolUse`).
struct Event {
    tool: String,
    args: Value,
    cwd: PathBuf,
    result: Option<String>,
}

impl Event {
    fn parse(v: &Value) -> Event {
        let field = |a: &str, b: &str| v.get(a).filter(|x| !x.is_null()).or_else(|| v.get(b)).cloned();
        let mut args = field("toolArgs", "tool_input").unwrap_or(Value::Null);
        // The documents show toolArgs as a JSON string; Copilot CLI 1.0 sends an object (and the
        // patch of apply_patch as a plain string).
        if let Value::String(s) = &args
            && let Ok(parsed @ Value::Object(_)) = serde_json::from_str::<Value>(s)
        {
            args = parsed;
        }
        let result = field("toolResult", "tool_result").and_then(|r| {
            r.get("textResultForLlm")
                .or_else(|| r.get("text_result_for_llm"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        Event {
            tool: field("toolName", "tool_name").and_then(|t| t.as_str().map(str::to_string)).unwrap_or_default(),
            args,
            cwd: field("cwd", "cwd").and_then(|c| c.as_str().map(PathBuf::from)).unwrap_or_default(),
            result,
        }
    }

    fn kind(&self) -> Kind {
        match self.tool.to_ascii_lowercase().as_str() {
            "view" | "read" | "read_file" | "readfile" => Kind::Read,
            "str_replace_editor" | "str_replace_based_edit_tool" if self.args["command"] == "view" => Kind::Read,
            "edit"
            | "create"
            | "write"
            | "multiedit"
            | "str_replace"
            | "str_replace_editor"
            | "str_replace_based_edit_tool"
            | "replace_string_in_file"
            | "multi_replace_string_in_file"
            | "insert_edit_into_file"
            | "create_file"
            | "write_file"
            | "edit_file"
            | "editfiles" => Kind::Write,
            "apply_patch" => Kind::Patch,
            "rg" | "grep" | "grep_search" => Kind::Search,
            "bash" | "powershell" | "pwsh" | "shell" | "run_in_terminal" | "runinterminal" | "execute" => Kind::Shell,
            _ => Kind::Other,
        }
    }

    fn resolve(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        if p.is_absolute() || self.cwd.as_os_str().is_empty() { p.to_path_buf() } else { self.cwd.join(p) }
    }

    /// The workspace the session works in, for paths outside every workspace folder (include
    /// folders outside the repositories).
    fn workspace(&self) -> Option<Workspace> {
        Workspace::find(&self.cwd).ok()
    }

    fn place(&self, abs: &Path) -> Option<Place> {
        locate::find(abs, None).or_else(|| self.workspace().and_then(|ws| locate::find(abs, Some(&ws))))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Read,
    Write,
    Patch,
    Search,
    Shell,
    Other,
}

const PATH_KEYS: [&str; 5] = ["path", "filePath", "file_path", "filepath", "file"];
const TEXT_KEYS: [&str; 11] = [
    "new_str",
    "new_string",
    "newString",
    "file_text",
    "content",
    "text",
    "code",
    "newText",
    "new_text",
    "insert_text",
    "contents",
];

/// Answers one hook event; `None` says nothing (Copilot goes on as usual).
pub fn hook(event: &str, input: &[u8]) -> Option<Value> {
    let v: Value = serde_json::from_slice(input).ok()?;
    match event {
        "sessionStart" | "SessionStart" => {
            let cwd = v.get("cwd").and_then(Value::as_str).map(PathBuf::from)?;
            session_context(&cwd).map(|c| json!({ "additionalContext": c }))
        }
        "preToolUse" | "PreToolUse" => pre_tool(&Event::parse(&v)),
        "postToolUse" | "PostToolUse" => post_tool(&Event::parse(&v)),
        _ => None,
    }
}

fn pre_tool(e: &Event) -> Option<Value> {
    match e.kind() {
        Kind::Read => redirect_read(e).map(|args| json!({ "modifiedArgs": args })),
        Kind::Write | Kind::Patch => {
            let reasons: Vec<String> = writes(e).iter().filter_map(|w| check_write(e, w)).collect();
            if reasons.is_empty() {
                return None;
            }
            record(e, "denied");
            Some(json!({ "permissionDecision": "deny", "permissionDecisionReason": reasons.join("\n") }))
        }
        _ => None,
    }
}

fn post_tool(e: &Event) -> Option<Value> {
    match e.kind() {
        Kind::Search => {
            let text = complete_search(e)?;
            Some(json!({ "modifiedResult": { "resultType": "success", "textResultForLlm": text } }))
        }
        Kind::Write | Kind::Patch => {
            for w in writes(e) {
                if let Some(place) = e.place(&e.resolve(&w.path))
                    && !place.in_mirror
                {
                    let _ = place.sync();
                }
            }
            None
        }
        Kind::Shell => {
            if let Some(ws) = e.workspace() {
                let _ = sync::sync(&ws, None);
            }
            None
        }
        _ => None,
    }
}

/// Reads of a file that is not UTF-8 go to its mirror (brought up to date first).
fn redirect_read(e: &Event) -> Option<Value> {
    let args = e.args.as_object()?;
    let key = PATH_KEYS.iter().find(|k| args.get(**k).is_some_and(Value::is_string))?;
    let abs = e.resolve(args[*key].as_str()?);
    if !abs.is_file() {
        return None;
    }
    let place = e.place(&abs)?;
    if place.in_mirror || reads_as_utf8(place.encoding()?) {
        return None;
    }
    let _ = place.sync();
    place.state()?.mirror.as_ref()?;
    let mut args = args.clone();
    args.insert((*key).to_string(), Value::String(paths::display(&place.mirror())));
    record(e, "read from the mirror");
    Some(Value::Object(args))
}

/// Encodings a UTF-8 reader reads right (the mirror of each is the same text).
fn reads_as_utf8(e: Encoding) -> bool {
    matches!(e, Encoding::Ascii | Encoding::Utf8 | Encoding::Utf8Bom | Encoding::AmbiguousUtf8Cp932)
}

/// A file a write tool would write, and the text it would put in.
#[derive(Debug, PartialEq)]
struct WriteTarget {
    path: String,
    text: String,
}

/// What a write tool's arguments would write.
fn writes(e: &Event) -> Vec<WriteTarget> {
    if e.kind() == Kind::Patch {
        let patch = match &e.args {
            Value::String(s) => s.clone(),
            v => ["input", "patch", "content"].iter().find_map(|k| v[k].as_str()).unwrap_or_default().to_string(),
        };
        return patch_writes(&patch);
    }
    let mut out = Vec::new();
    collect_writes(&e.args, None, &mut out);
    if out.is_empty() {
        // Arguments this hook does not know: every string that names a file, with all the text.
        let mut strings = Vec::new();
        all_strings(&e.args, &mut strings);
        let text = strings.join("\n");
        for s in &strings {
            if s.len() < 1024 && !s.contains('\n') && (s.contains('\\') || s.contains('/')) {
                out.push(WriteTarget { path: s.clone(), text: text.clone() });
            }
        }
    }
    out
}

fn collect_writes(v: &Value, inherited: Option<&str>, out: &mut Vec<WriteTarget>) {
    match v {
        Value::Object(map) => {
            let path = PATH_KEYS.iter().find_map(|k| map.get(*k).and_then(Value::as_str)).or(inherited);
            let text: Vec<&str> = TEXT_KEYS.iter().filter_map(|k| map.get(*k).and_then(Value::as_str)).collect();
            if let Some(path) = path
                && (!text.is_empty() || map.contains_key("path") || map.contains_key("filePath"))
                && inherited.is_none_or(|i| i != path || !text.is_empty())
            {
                out.push(WriteTarget { path: path.to_string(), text: text.join("\n") });
            }
            for (k, child) in map {
                if matches!(child, Value::Array(_) | Value::Object(_)) && !TEXT_KEYS.contains(&k.as_str()) {
                    collect_writes(child, path, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| collect_writes(i, inherited, out)),
        _ => {}
    }
}

fn all_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| all_strings(x, out)),
        Value::Object(m) => m.values().for_each(|x| all_strings(x, out)),
        _ => {}
    }
}

/// The files of an `apply_patch` patch and the lines it adds to each.
fn patch_writes(patch: &str) -> Vec<WriteTarget> {
    let mut out: Vec<WriteTarget> = Vec::new();
    for line in patch.lines() {
        let line = line.trim_end_matches('\r');
        let header = ["*** Update File:", "*** Add File:", "*** Delete File:", "*** Move to:"]
            .iter()
            .find_map(|h| line.strip_prefix(h));
        if let Some(path) = header {
            out.push(WriteTarget { path: path.trim().to_string(), text: String::new() });
        } else if let Some(added) = line.strip_prefix('+')
            && let Some(last) = out.last_mut()
        {
            last.text.push_str(added);
            last.text.push('\n');
        }
    }
    out
}

/// Why a built-in tool must not write this file, if it must not.
fn check_write(e: &Event, w: &WriteTarget) -> Option<String> {
    let abs = e.resolve(&w.path);
    let place = e.place(&abs)?;
    let shown = paths::display(&place.original());
    if place.in_mirror {
        return Some(format!(
            "mixenc: {} is a mirror (read-only, made from the original). Change the original {shown} with the mixenc MCP server's edit tool (it takes the mirror's path too).",
            paths::display(&abs)
        ));
    }
    let bytes = fs::read(place.original()).ok();
    let encoding = match (place.read_as(), &bytes) {
        (Some(ReadAs::Encoding(e)), Some(_)) => Some(e),
        (_, Some(b)) => Some(judge::judge(b)),
        (_, None) => None,
    };
    let gate = "Use the mixenc MCP server's edit tool (old_string and new_string, as this tool takes them) or create for a new file: it writes the file in its own encoding and keeps every line you did not change byte for byte.";
    match encoding {
        Some(Encoding::Utf8 | Encoding::Utf8Bom) => None,
        Some(Encoding::AmbiguousUtf8Cp932) if w.text.is_ascii() => None,
        Some(Encoding::AmbiguousUtf8Cp932) => Some(format!(
            "mixenc: {shown} reads both as UTF-8 and as CP932, and which it is has not been decided (mixenc.json, overrides). Write only ASCII into it until then."
        )),
        None | Some(Encoding::Ascii) if w.text.is_ascii() => None,
        None | Some(Encoding::Ascii) => {
            let pick = Defaults::load(&place.ws, &place.at.repo).for_file(&place.rel);
            match pick.choice {
                Choice::Write(Encoding::Utf8) => None,
                Choice::Write(enc) => Some(format!(
                    "mixenc: {shown} would get non-ASCII text, and this tool writes UTF-8 without a BOM, which MSVC reads as CP932 here: the files around it are {enc} ({}). {gate}",
                    pick.why
                )),
                Choice::Ask => Some(format!(
                    "mixenc: {shown} would get non-ASCII text, and which encoding it should be is for the user to decide ({}). Ask the user, then use the mixenc MCP server's edit or create tool with encoding.",
                    pick.why
                )),
            }
        }
        Some(enc) => Some(format!(
            "mixenc: {shown} is {enc}, and this tool would write it as UTF-8, changing the bytes of lines you did not touch. {gate} Read it with view (mixenc shows its UTF-8 mirror) or the mixenc tool read_lines."
        )),
    }
}

/// grep's answer with the matches in files that are not UTF-8 (searched in their mirrors, named
/// by their originals) in place of what grep read wrong in them. `None` when nothing changes.
fn complete_search(e: &Event) -> Option<String> {
    let args = e.args.as_object()?;
    let pattern = args.get("pattern").and_then(Value::as_str)?;
    let scopes: Vec<String> = match args.get("paths").or_else(|| args.get("path")) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect(),
        _ => vec![String::new()],
    };
    let flag = |names: &[&str]| names.iter().any(|n| args.get(*n).and_then(Value::as_bool) == Some(true));
    let strings = |names: &[&str]| -> Vec<String> {
        names
            .iter()
            .filter_map(|n| args.get(*n))
            .flat_map(|v| match v {
                Value::String(s) => vec![s.clone()],
                Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
                _ => Vec::new(),
            })
            .collect()
    };
    let opts = Options {
        ignore_case: flag(&["-i", "ignore_case", "case_insensitive", "ignoreCase"]),
        fixed: flag(&["-F", "fixed_strings", "fixedStrings", "literal"]),
        globs: strings(&["glob", "globs", "include", "includePattern"]),
        types: strings(&["type", "types"]),
    };
    let m = Matcher::new(pattern, &opts).ok()?;
    let number = |names: &[&str]| names.iter().find_map(|n| args.get(*n).and_then(Value::as_u64)).unwrap_or(0) as usize;
    let context = number(&["-C", "context"]);
    let (before, after) = (number(&["-B"]).max(context), number(&["-A"]).max(context));
    let mode = args.get("output_mode").and_then(Value::as_str).unwrap_or("files_with_matches");
    let numbered = args.get("-n").and_then(Value::as_bool).unwrap_or(true);

    // The files grep read wrong, with how grep names them.
    let mut redone: Vec<(String, PathBuf, Vec<String>)> = Vec::new();
    for scope in &scopes {
        let abs = e.resolve(if scope.is_empty() { "." } else { scope });
        let ws =
            locate::find(&abs, None).map(|p| p.ws).or_else(|| Workspace::find(&abs).ok()).or_else(|| e.workspace())?;
        let abs_s = paths::canonical(&abs).to_string_lossy().into_owned();
        for at in locate::roots(&ws) {
            let source = at.root.source.to_string_lossy().into_owned();
            let prefix = match paths::rest_under(&abs_s, &source) {
                Some(rest) => rest.replace('\\', "/"),
                None if paths::is_under(&source, &abs_s) => String::new(),
                None => continue,
            };
            for (rel, enc) in search::mirrored(&at.root) {
                if reads_as_utf8(enc) || !(prefix.is_empty() || rel == prefix || rel.starts_with(&format!("{prefix}/")))
                {
                    continue;
                }
                let original = paths::join_rel(&at.root.source, &rel);
                let under =
                    paths::rest_under(&original.to_string_lossy(), &abs_s).unwrap_or_default().replace('\\', "/");
                if !m.wants(if under.is_empty() { &rel } else { &under }) {
                    continue;
                }
                let shown = grep_name(scope, &abs_s, &original);
                let hits = search::file_lines(&paths::join_rel(&at.root.mirror, &rel));
                redone.push((shown, original, hits));
            }
        }
    }
    if redone.is_empty() {
        return None;
    }

    let mut ours: Vec<String> = Vec::new();
    let mut found_files = 0;
    for (shown, _, lines) in &redone {
        let matching: Vec<usize> =
            lines.iter().enumerate().filter(|(_, l)| !m.lines(l.as_bytes()).is_empty()).map(|(i, _)| i).collect();
        if matching.is_empty() {
            continue;
        }
        found_files += 1;
        match mode {
            "content" => {
                let mut groups: Vec<(usize, usize)> = Vec::new();
                for &i in &matching {
                    let (start, end) = (i.saturating_sub(before), (i + after).min(lines.len() - 1));
                    match groups.last_mut() {
                        Some(g) if start <= g.1 + 1 => g.1 = g.1.max(end),
                        _ => groups.push((start, end)),
                    }
                }
                for (start, end) in groups {
                    // ripgrep puts -- between groups of lines with context.
                    if before + after > 0 && !ours.is_empty() {
                        ours.push("--".into());
                    }
                    for (i, line) in lines.iter().enumerate().take(end + 1).skip(start) {
                        let sep = if matching.contains(&i) { ':' } else { '-' };
                        ours.push(if numbered {
                            format!("{shown}{sep}{}{sep}{line}", i + 1)
                        } else {
                            format!("{shown}{sep}{line}")
                        });
                    }
                }
            }
            "count" => ours.push(format!("{shown}:{}", matching.len())),
            _ => ours.push(shown.clone()),
        }
    }

    let original = e.result.as_deref().unwrap_or_default();
    let names: Vec<String> = redone.iter().map(|(s, _, _)| s.to_lowercase().replace('/', "\\")).collect();
    let belongs = |line: &str| {
        let l = line.to_lowercase().replace('/', "\\");
        names.iter().any(|n| {
            l.strip_prefix(n.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(':') || rest.starts_with('-'))
        })
    };
    let mut kept: Vec<&str> = Vec::new();
    let mut removed = 0;
    for line in original.lines() {
        if line.trim() == "No matches found." {
            continue;
        }
        if belongs(line) {
            removed += 1;
        } else {
            kept.push(line);
        }
    }
    if ours.is_empty() && removed == 0 {
        return None;
    }
    // Separators left at the ends or doubled once the lines between them are gone.
    let mut lines: Vec<String> = Vec::new();
    for line in kept {
        if line == "--" && lines.last().is_none_or(|l| l == "--") {
            continue;
        }
        lines.push(line.to_string());
    }
    while lines.last().is_some_and(|l| l == "--") {
        lines.pop();
    }
    if !lines.is_empty() && !ours.is_empty() && mode == "content" && before + after > 0 {
        lines.push("--".into());
    }
    lines.extend(ours);
    if let Some(limit) = args.get("head_limit").and_then(Value::as_u64).filter(|l| *l > 0) {
        lines.truncate(limit as usize);
    }
    let mut text = if lines.iter().all(|l| l.is_empty()) { "No matches found.".to_string() } else { lines.join("\n") };
    text.push_str(&format!(
        "\n(mixenc: {} here {} CP932 or another encoding grep reads wrong, searched in the UTF-8 mirror{})",
        if redone.len() == 1 { "1 file".to_string() } else { format!("{} files", redone.len()) },
        if redone.len() == 1 { "is" } else { "are" },
        if found_files > 0 { format!(", {found_files} matched") } else { String::new() }
    ));
    record(e, "search completed");
    Some(text)
}

/// How ripgrep names a file found under a search path given as `scope`.
fn grep_name(scope: &str, scope_abs: &str, file: &Path) -> String {
    let rest = paths::rest_under(&file.to_string_lossy(), scope_abs).unwrap_or_default();
    let scope = scope.trim_end_matches(['/', '\\']);
    if scope.is_empty() || scope == "." {
        format!(".\\{rest}")
    } else if rest.is_empty() {
        scope.to_string()
    } else {
        format!("{scope}\\{rest}")
    }
}

/// The context a session in a workspace starts with: how to read and write here, and the doctor's
/// short report (if it finishes in time).
fn session_context(cwd: &Path) -> Option<String> {
    let ws = Workspace::find(cwd).ok()?;
    let names: Vec<String> = ws.repos().into_iter().map(|r| r.name).collect();
    let mut text = format!(
        "This is a mixenc workspace ({}; repositories: {}). Its C/C++ files may be CP932 (Shift_JIS), a mix of encodings, or UTF-16, not UTF-8:\n\
         - Reading: view/read of such a file shows its UTF-8 mirror (the same lines), and grep's answers include them read right. The mixenc MCP tools read_lines and search do the same.\n\
         - Writing: change them only with the mixenc MCP tools edit and create, which keep each file's encoding and every untouched line's bytes. Built-in edit, create and apply_patch are stopped for them; shell commands that write them are stopped by the commit gate.\n\
         - Code structure: ask clangd through the mixenc MCP tools (find_symbol, definition, references, call_hierarchy, type_hierarchy, outline), or hand the question to the mixenc-analyst agent, before searching text.",
        paths::display(&ws.root),
        names.join(", ")
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let doctor_ws = ws.clone();
    std::thread::spawn(move || {
        let _ = tx.send(doctor::run(&doctor_ws));
    });
    match rx.recv_timeout(Duration::from_secs(12)) {
        Ok(report) => {
            let s = &report.summary;
            text.push_str(&format!("\nmixenc doctor: {} broken, {} notices.", s.broken, s.notice));
            for item in report.items.iter().filter(|i| i.severity != doctor::Severity::Ok).take(8) {
                let repo = item.repo.as_deref().map(|r| format!("{r}: ")).unwrap_or_default();
                text.push_str(&format!("\n- {repo}{}", item.message));
            }
        }
        Err(_) => {
            text.push_str("\nmixenc doctor did not finish in 12 s; run `mixenc doctor` to see the workspace's state.")
        }
    }
    Some(text)
}

/// A line in the workspace's hook log (`mirror/.mixenc/copilot.log`, on this PC only).
fn record(e: &Event, what: &str) {
    let Some(ws) = e.workspace() else { return };
    let dir = ws.state_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("copilot.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = fs::remove_file(&path);
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{now} {} {what}", e.tool);
    }
}

// ---------------------------------------------------------------------------------------------
// Installing

/// Where Copilot keeps its user settings: `COPILOT_HOME`, else `~/.copilot`.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("COPILOT_HOME") {
        return PathBuf::from(h);
    }
    let profile = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
    PathBuf::from(profile).join(".copilot")
}

/// The hooks file for mixenc at `exe`.
pub fn hooks_json(exe: &Path) -> Value {
    let exe = paths::display(exe);
    let mut hooks = Map::new();
    for (event, timeout) in EVENTS {
        hooks.insert(
            event.to_string(),
            json!([{ "type": "command", "exec": exe, "args": ["copilot", "hook", event], "timeoutSec": timeout }]),
        );
    }
    json!({ "version": 1, "hooks": hooks })
}

/// One piece of the installation and how it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub name: &'static str,
    pub path: PathBuf,
    /// Installed and pointing at an existing mixenc.
    pub ok: bool,
    pub text: String,
}

pub fn status(home: &Path) -> Vec<Piece> {
    let mut out = Vec::new();
    let hooks = home.join("hooks").join(HOOKS_FILE);
    let exe =
        read_json(&hooks).ok().flatten().and_then(|v| v["hooks"]["preToolUse"][0]["exec"].as_str().map(PathBuf::from));
    out.push(match exe {
        Some(exe) if exe.is_file() => {
            Piece { name: "hooks", path: hooks, ok: true, text: format!("installed ({})", paths::display(&exe)) }
        }
        Some(exe) => Piece {
            name: "hooks",
            path: hooks,
            ok: false,
            text: format!(
                "installed, but {} is gone: Copilot denies every tool until it is back or the hooks are uninstalled",
                paths::display(&exe)
            ),
        },
        None => Piece { name: "hooks", path: hooks, ok: false, text: "not installed".into() },
    });
    let agent = home.join("agents").join(AGENT_FILE);
    let agent_ok = fs::read_to_string(&agent).is_ok_and(|t| t == AGENT);
    out.push(Piece {
        name: "agent",
        text: if agent_ok {
            "installed"
        } else if agent.is_file() {
            "installed, another version"
        } else {
            "not installed"
        }
        .into(),
        path: agent,
        ok: agent_ok,
    });
    let mcp = home.join("mcp-config.json");
    let server = read_json(&mcp).ok().flatten().map(|v| v["mcpServers"]["mixenc"].clone()).filter(|s| !s.is_null());
    out.push(match server {
        Some(s) => {
            let exe = s["command"].as_str().map(PathBuf::from);
            let ok = exe.as_ref().is_some_and(|e| e.is_file()) && s["args"][0] == "mcp";
            Piece {
                name: "mcp",
                path: mcp,
                ok,
                text: if ok { "registered" } else { "registered, but not as this mixenc runs it" }.into(),
            }
        }
        None => Piece { name: "mcp", path: mcp, ok: false, text: "not registered".into() },
    });
    let lsp = home.join("lsp-config.json");
    let clangd = read_json(&lsp).ok().flatten().map(|v| v["lspServers"]["clangd"].clone()).filter(|s| !s.is_null());
    out.push(match clangd {
        Some(c) if c["args"][0] == "clangd" && c["command"].as_str().is_some_and(|e| Path::new(e).is_file()) => {
            Piece { name: "lsp", path: lsp, ok: true, text: "clangd goes through the entry".into() }
        }
        Some(_) => Piece { name: "lsp", path: lsp, ok: false, text: "clangd is not the entry".into() },
        None => Piece { name: "lsp", path: lsp, ok: false, text: "no clangd".into() },
    });
    out
}

/// Installs the hooks, the analysis agent, the MCP server and (with `lsp`) the entry as Copilot
/// CLI's clangd. Settings of other tools in the same files are kept; clangd's entry before is
/// kept in `mixenc-install.json` for `uninstall`.
pub fn install(home: &Path, exe: &Path, lsp: bool) -> Result<Vec<String>> {
    let exe = paths::canonical(exe);
    let mut done = Vec::new();
    let hooks = home.join("hooks").join(HOOKS_FILE);
    write_text(&hooks, &(serde_json::to_string_pretty(&hooks_json(&exe))? + "\n"))?;
    done.push(format!("hooks: {}", paths::display(&hooks)));
    let agent = home.join("agents").join(AGENT_FILE);
    write_text(&agent, AGENT)?;
    done.push(format!("agent: {}", paths::display(&agent)));

    let mcp = home.join("mcp-config.json");
    let mut config = read_json(&mcp)?.unwrap_or_else(|| json!({}));
    let servers = object(&mut config, "mcpServers")?;
    servers.insert(
        "mixenc".into(),
        json!({ "type": "local", "command": paths::display(&exe), "args": ["mcp"], "tools": ["*"] }),
    );
    write_text(&mcp, &(serde_json::to_string_pretty(&config)? + "\n"))?;
    done.push(format!("mcp: {} (server mixenc)", paths::display(&mcp)));

    if lsp {
        let path = home.join("lsp-config.json");
        let mut config = read_json(&path)?.unwrap_or_else(|| json!({}));
        let servers = object(&mut config, "lspServers")?;
        let before = servers.get("clangd").cloned();
        let ours = before.as_ref().is_some_and(|b| {
            b["args"][0] == "clangd" && b["command"].as_str().is_some_and(|c| c.to_ascii_lowercase().contains("mixenc"))
        });
        let mut entry = before.clone().filter(|b| b.is_object()).unwrap_or_else(|| {
            json!({ "fileExtensions": { ".c": "c", ".h": "cpp", ".cpp": "cpp", ".cc": "cpp", ".cxx": "cpp", ".hpp": "cpp", ".hxx": "cpp", ".inl": "cpp" } })
        });
        let mut args: Vec<Value> = entry["args"].as_array().cloned().unwrap_or_default();
        if ours {
            args.remove(0);
        } else {
            let record_path = home.join(RECORD_FILE);
            let mut record = read_json(&record_path)?.unwrap_or_else(|| json!({}));
            record["lsp_clangd_before"] = before.clone().unwrap_or(Value::Null);
            write_text(&record_path, &(serde_json::to_string_pretty(&record)? + "\n"))?;
        }
        args.insert(0, json!("clangd"));
        entry["command"] = json!(paths::display(&exe));
        entry["args"] = Value::Array(args);
        servers.insert("clangd".into(), entry);
        write_text(&path, &(serde_json::to_string_pretty(&config)? + "\n"))?;
        done.push(format!("lsp: {} (clangd goes through the entry)", paths::display(&path)));
    }
    Ok(done)
}

/// Takes out what `install` put in, and puts clangd's entry back as it was.
pub fn uninstall(home: &Path) -> Result<Vec<String>> {
    let mut done = Vec::new();
    for path in [home.join("hooks").join(HOOKS_FILE), home.join("agents").join(AGENT_FILE)] {
        if path.is_file() {
            fs::remove_file(&path).with_context(|| format!("removing {}", paths::display(&path)))?;
            done.push(format!("removed {}", paths::display(&path)));
        }
    }
    let mcp = home.join("mcp-config.json");
    if let Some(mut config) = read_json(&mcp)?
        && config["mcpServers"].as_object_mut().and_then(|s| s.remove("mixenc")).is_some()
    {
        write_text(&mcp, &(serde_json::to_string_pretty(&config)? + "\n"))?;
        done.push(format!("removed the server mixenc from {}", paths::display(&mcp)));
    }
    let record_path = home.join(RECORD_FILE);
    let lsp = home.join("lsp-config.json");
    if let Some(record) = read_json(&record_path)?
        && let Some(mut config) = read_json(&lsp)?
        && config["lspServers"]["clangd"]["args"][0] == "clangd"
    {
        let servers = object(&mut config, "lspServers")?;
        match &record["lsp_clangd_before"] {
            Value::Null => {
                servers.remove("clangd");
            }
            before => {
                servers.insert("clangd".into(), before.clone());
            }
        }
        write_text(&lsp, &(serde_json::to_string_pretty(&config)? + "\n"))?;
        fs::remove_file(&record_path)?;
        done.push(format!("put clangd back as it was in {}", paths::display(&lsp)));
    }
    Ok(done)
}

fn read_json(path: &Path) -> Result<Option<Value>> {
    match fs::read(path) {
        Ok(bytes) => {
            let bytes = bytes.strip_prefix(crate::text::UTF8_BOM).unwrap_or(&bytes);
            Ok(Some(serde_json::from_slice(bytes).with_context(|| format!("reading {}", paths::display(path)))?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", paths::display(path))),
    }
}

fn object<'a>(v: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>> {
    if v.get(key).is_none_or(Value::is_null) {
        v[key] = json!({});
    }
    v[key].as_object_mut().with_context(|| format!("\"{key}\" is not an object"))
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("a file without a folder")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", paths::display(dir)))?;
    fsx::write_replace(dir, path, text.as_bytes(), false).with_context(|| format!("writing {}", paths::display(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_argument_forms() {
        let camel = json!({ "toolName": "view", "toolArgs": { "path": "a.cpp" }, "cwd": "C:\\w" });
        let e = Event::parse(&camel);
        assert_eq!((e.tool.as_str(), e.args["path"].as_str(), e.kind()), ("view", Some("a.cpp"), Kind::Read));
        let string = json!({ "toolName": "edit", "toolArgs": "{\"path\":\"a.cpp\",\"new_str\":\"x\"}" });
        assert_eq!(Event::parse(&string).args["new_str"], "x");
        let snake = json!({ "tool_name": "Edit", "tool_input": { "file_path": "a.cpp", "new_string": "表" } });
        let e = Event::parse(&snake);
        assert_eq!(writes(&e), vec![WriteTarget { path: "a.cpp".into(), text: "表".into() }]);
        let post = json!({ "toolName": "rg", "toolArgs": {}, "toolResult": { "resultType": "success", "textResultForLlm": "x" } });
        assert_eq!(Event::parse(&post).result.as_deref(), Some("x"));
    }

    #[test]
    fn reads_a_patch() {
        let patch = "*** Begin Patch\n*** Update File: C:\\r\\a.cpp\n@@\n int a;\n+// 表\n*** Add File: b.h\n+#pragma once\n*** Delete File: c.h\n*** End Patch\n";
        let e = Event { tool: "apply_patch".into(), args: json!(patch), cwd: PathBuf::new(), result: None };
        assert_eq!(
            writes(&e),
            vec![
                WriteTarget { path: "C:\\r\\a.cpp".into(), text: "// 表\n".into() },
                WriteTarget { path: "b.h".into(), text: "#pragma once\n".into() },
                WriteTarget { path: "c.h".into(), text: String::new() },
            ]
        );
    }

    #[test]
    fn finds_writes_in_lists_and_unknown_shapes() {
        let e = Event {
            tool: "multi_replace_string_in_file".into(),
            args: json!({ "replacements": [{ "filePath": "a.cpp", "newString": "x" }, { "filePath": "b.h", "newString": "表" }] }),
            cwd: PathBuf::new(),
            result: None,
        };
        assert_eq!(writes(&e).iter().map(|w| w.path.as_str()).collect::<Vec<_>>(), ["a.cpp", "b.h"]);
        let unknown = Event {
            tool: "edit".into(),
            args: json!({ "target": "src/a.cpp", "body": "表" }),
            cwd: PathBuf::new(),
            result: None,
        };
        assert_eq!(writes(&unknown), vec![WriteTarget { path: "src/a.cpp".into(), text: "表\nsrc/a.cpp".into() }]);
    }

    #[test]
    fn names_files_as_grep_does() {
        let f = Path::new(r"C:\w\r\src\a.cpp");
        assert_eq!(grep_name("src", r"C:\w\r\src", f), r"src\a.cpp");
        assert_eq!(grep_name("", r"C:\w\r", f), r".\src\a.cpp");
        assert_eq!(grep_name("src/a.cpp", r"C:\w\r\src\a.cpp", f), "src/a.cpp");
    }

    #[test]
    fn the_hooks_file_runs_this_exe() {
        let v = hooks_json(Path::new(r"C:\tools\mixenc.exe"));
        assert_eq!(v["hooks"]["preToolUse"][0]["exec"], r"C:\tools\mixenc.exe");
        assert_eq!(v["hooks"]["preToolUse"][0]["args"], json!(["copilot", "hook", "preToolUse"]));
        assert!(v["hooks"]["sessionStart"].is_array() && v["hooks"]["postToolUse"].is_array());
    }
}
