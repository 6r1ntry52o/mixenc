//! The entry, `mixenc clangd`: stands between an editor or an agent and clangd.
//!
//! - At the start (the client's `initialize`): finds the workspace from the client's folders, syncs
//!   the repositories, and starts clangd on the mirror (`--compile-commands-dir`, `--path-mappings`).
//!   Without a workspace it passes everything through.
//! - On the way: before `didOpen` and `didSave`, mirrors that file.
//! - A client that read a CP932 original as UTF-8 (agents' LSP clients do) sends a text with the
//!   0x5C trap in it: such a text is replaced with the mirror's, and the columns of that client's
//!   positions are moved between its reading and the mirror's from then on (the lines are the same).
//! - Watches the repositories (and the database) while clangd runs, and syncs after changes.
//! - Tells the client (`window/showMessage`) when files are not mirrored; analysis goes on.
//! - When the entry itself fails on a message, it passes that message through as it came.

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::colmap::{FileMap, Units, Way};
use crate::config::{RepoPaths, Workspace};
use crate::index::Problem;
use crate::judge::{self, Encoding};
use crate::mirror::{self, Root};
use crate::{lsp, paths, sync, translate, uri};

/// clangd options the entry sets itself.
const OWN_OPTIONS: [&str; 2] = ["--compile-commands-dir", "--path-mappings"];

/// Runs the entry with clangd's arguments; returns clangd's exit code.
pub fn run(args: Vec<String>) -> Result<i32> {
    // `--version`, `--help`, `--check`: not a language server session.
    if args.iter().any(|a| a == "--version" || a == "--help" || a == "-help" || a.starts_with("--check")) {
        let clangd = find_clangd(None)?;
        let status = Command::new(clangd).args(&args).status()?;
        return Ok(status.code().unwrap_or(1));
    }
    let mut from_client = BufReader::new(io::stdin().lock());
    let Some(first) = lsp::read(&mut from_client)? else { return Ok(0) };
    let initialize: Value = serde_json::from_slice(&first).unwrap_or(Value::Null);

    let workspace = client_folders(&initialize).iter().find_map(|dir| Workspace::find(dir).ok());
    let Some(ws) = workspace else {
        log("no mixenc.json above the client's folders: passing everything through to clangd");
        return pass_through(&args, &first, from_client);
    };
    let session = Session::start(ws, &initialize, &args)?;
    session.serve(first, from_client)
}

/// The folders the client opened: `workspaceFolders`, `rootUri`, `rootPath`, then the current one.
fn client_folders(initialize: &Value) -> Vec<PathBuf> {
    let params = &initialize["params"];
    let mut dirs = Vec::new();
    if let Some(folders) = params["workspaceFolders"].as_array() {
        dirs.extend(folders.iter().filter_map(|f| f["uri"].as_str()).filter_map(uri::to_path));
    }
    dirs.extend(params["rootUri"].as_str().and_then(uri::to_path));
    dirs.extend(params["rootPath"].as_str().map(PathBuf::from));
    dirs.extend(std::env::current_dir().ok());
    dirs
}

/// The clangd to start: `MIXENC_CLANGD`, `clangd` in mixenc.json, `clangd.exe` on PATH (not this
/// program), or the one vscode-clangd installed.
pub fn find_clangd(ws: Option<&Workspace>) -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("MIXENC_CLANGD") {
        return Ok(PathBuf::from(p));
    }
    if let Some(p) = ws.and_then(|w| w.config.clangd.as_deref()) {
        return Ok(ws.map_or_else(|| PathBuf::from(p), |w| w.resolve(p)));
    }
    let me = std::env::current_exe().ok().map(|p| paths::key(&p.to_string_lossy()));
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = dir.join("clangd.exe");
        if candidate.is_file() && Some(paths::key(&candidate.to_string_lossy())) != me {
            return Ok(candidate);
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let install =
            PathBuf::from(appdata).join(r"Code\User\globalStorage\llvm-vs-code-extensions.vscode-clangd\install");
        let mut found: Vec<PathBuf> = std::fs::read_dir(&install)
            .into_iter()
            .flatten()
            .flatten()
            .flat_map(|version| std::fs::read_dir(version.path()).into_iter().flatten().flatten())
            .map(|d| d.path().join(r"bin\clangd.exe"))
            .filter(|p| p.is_file())
            .collect();
        found.sort();
        if let Some(p) = found.pop() {
            return Ok(p);
        }
    }
    bail!("clangd not found: set MIXENC_CLANGD, \"clangd\" in mixenc.json, or put clangd.exe on PATH")
}

fn log(message: &str) {
    eprintln!("mixenc: {message}");
}

/// No workspace: clangd as it is, with the first message already read.
fn pass_through(args: &[String], first: &[u8], mut from_client: impl io::BufRead) -> Result<i32> {
    let mut child = Command::new(find_clangd(None)?).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
    let mut to_server = child.stdin.take().context("clangd's stdin")?;
    let mut from_server = child.stdout.take().context("clangd's stdout")?;
    std::thread::spawn(move || pump(&mut from_server, &mut io::stdout().lock()));
    lsp::write(&mut to_server, first)?;
    pump(&mut from_client, &mut to_server);
    drop(to_server);
    Ok(child.wait()?.code().unwrap_or(1))
}

/// Copies until the end, flushing every read: stdout is line-buffered, and an LSP message does not
/// end with a line break.
fn pump(from: &mut impl io::Read, to: &mut impl io::Write) {
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if to.write_all(&buffer[..n]).and_then(|()| to.flush()).is_err() {
                    return;
                }
            }
        }
    }
}

/// A document the entry translates: the client sent the original misread as UTF-8.
struct Translated {
    map: Arc<FileMap>,
    /// The text as the client has it (to apply its changes to).
    client_text: String,
}

#[derive(Default)]
struct State {
    units: Units,
    /// The client sent a misread text once: its columns are moved from then on.
    misreading: bool,
    translated: HashMap<String, Translated>,
    /// Requests from the client: id → (method, document).
    pending: HashMap<String, (String, Option<String>)>,
    /// Column maps of files on disk, by path: (size, time, map).
    disk: HashMap<PathBuf, (u64, u64, Option<Arc<FileMap>>)>,
    /// Messages for the client once `initialize` is answered.
    notices: Vec<String>,
    initialized: bool,
    told_broken: bool,
    told_unknown_text: bool,
}

struct Session {
    ws: Workspace,
    repos: Vec<RepoPaths>,
    /// Every mirrored root, the most specific first.
    roots: Vec<Root>,
    state: Mutex<State>,
    to_client: Mutex<io::Stdout>,
    to_server: Mutex<Option<ChildStdin>>,
    child: Mutex<Child>,
    from_server: Mutex<Option<std::process::ChildStdout>>,
}

impl Session {
    fn start(ws: Workspace, initialize: &Value, args: &[String]) -> Result<Arc<Session>> {
        let folders = client_folders(initialize);
        let all = ws.repos();
        let mut repos: Vec<RepoPaths> = all
            .iter()
            .filter(|r| {
                folders.iter().any(|f| {
                    let (f, s) = (f.to_string_lossy(), r.source.to_string_lossy());
                    paths::is_under(&f, &s) || paths::is_under(&s, &f)
                })
            })
            .cloned()
            .collect();
        if repos.is_empty() {
            repos = all;
        }

        let mut notices = Vec::new();
        let mut mappings: Vec<String> = Vec::new();
        let mut roots: Vec<Root> = Vec::new();
        for repo in &repos {
            match sync::sync_repo(&ws, repo) {
                Ok(result) => {
                    notices.extend(problems_of(&result));
                    roots.extend(result.state.roots());
                    mappings
                        .extend(result.state.path_mappings.split(',').filter(|m| !m.is_empty()).map(str::to_string));
                }
                Err(err) => {
                    notices.push(format!(
                        "mixenc could not sync {}: {err:#}. clangd reads the mirror as it was.",
                        repo.name
                    ));
                    if let Some(state) = sync::RepoState::load(&repo.mirror) {
                        roots.extend(state.roots());
                        mappings.extend(state.path_mappings.split(',').map(str::to_string));
                    }
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        roots.retain(|r| seen.insert(paths::key(&r.source.to_string_lossy())));
        roots.sort_by_key(|r| std::cmp::Reverse(r.source.as_os_str().len()));
        // One list over every repository's roots, the most specific first.
        if !roots.is_empty() {
            mappings = vec![crate::db::Mapper::new(&roots).path_mappings()];
        }

        let mut clangd_args: Vec<String> =
            args.iter().filter(|a| !OWN_OPTIONS.iter().any(|o| a.starts_with(o))).cloned().collect();
        if let [repo] = repos.as_slice() {
            clangd_args.push(format!("--compile-commands-dir={}", paths::display(&repo.mirror)));
        }
        if !mappings.is_empty() {
            clangd_args.push(format!("--path-mappings={}", mappings.join(",")));
        }
        let clangd = find_clangd(Some(&ws))?;
        log(&format!(
            "workspace {}, repositories [{}], clangd {}",
            paths::display(&ws.root),
            repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", "),
            paths::display(&clangd)
        ));
        let mut child = Command::new(&clangd)
            .args(&clangd_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", paths::display(&clangd)))?;
        let to_server = child.stdin.take().context("clangd's stdin")?;
        let from_server = child.stdout.take().context("clangd's stdout")?;
        Ok(Arc::new(Session {
            ws,
            repos,
            roots,
            state: Mutex::new(State { notices, ..State::default() }),
            to_client: Mutex::new(io::stdout()),
            to_server: Mutex::new(Some(to_server)),
            child: Mutex::new(child),
            from_server: Mutex::new(Some(from_server)),
        }))
    }

    fn serve(self: &Arc<Self>, first: Vec<u8>, mut from_client: impl io::BufRead) -> Result<i32> {
        let me = Arc::clone(self);
        let from_server = self.from_server.lock().unwrap().take().context("clangd's stdout")?;
        let server_thread = std::thread::spawn(move || me.serve_server(from_server));
        let me = Arc::clone(self);
        std::thread::spawn(move || me.watch());

        let mut body = Some(first);
        while let Some(message) = body.take().map_or_else(|| lsp::read(&mut from_client), |b| Ok(Some(b)))? {
            let out = match catch_unwind(AssertUnwindSafe(|| self.client_message(&message))) {
                Ok(out) => out,
                Err(_) => {
                    self.tell_broken();
                    message
                }
            };
            let mut to_server = self.to_server.lock().unwrap();
            let Some(to_server) = to_server.as_mut() else { break };
            if lsp::write(to_server, &out).is_err() {
                break;
            }
        }
        // The client is gone: so is clangd's input. clangd ends after `exit`; give it a moment.
        drop(self.to_server.lock().unwrap().take());
        for _ in 0..50 {
            if let Ok(Some(status)) = self.child.lock().unwrap().try_wait() {
                return Ok(status.code().unwrap_or(0));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        drop(server_thread);
        Ok(child.wait().ok().and_then(|s| s.code()).unwrap_or(1))
    }

    fn serve_server(self: Arc<Self>, from_server: std::process::ChildStdout) {
        let mut reader = BufReader::new(from_server);
        while let Ok(Some(message)) = lsp::read(&mut reader) {
            let out = match catch_unwind(AssertUnwindSafe(|| self.server_message(&message))) {
                Ok(out) => out,
                Err(_) => {
                    self.tell_broken();
                    message
                }
            };
            if self.send_client(&out).is_err() {
                break;
            }
            self.flush_notices();
        }
        // clangd ended: end with it, so that the client sees the server go and can start it again.
        let code = loop {
            if let Ok(Some(status)) = self.child.lock().unwrap().try_wait() {
                break status.code().unwrap_or(1);
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        std::process::exit(code);
    }

    fn send_client(&self, body: &[u8]) -> io::Result<()> {
        let out = self.to_client.lock().unwrap();
        lsp::write(&mut out.lock(), body)
    }

    fn notify_client(&self, message: &str) {
        let body = json!({"jsonrpc": "2.0", "method": "window/showMessage", "params": {"type": 2, "message": message}});
        let _ = self.send_client(body.to_string().as_bytes());
    }

    fn flush_notices(&self) {
        let notices = {
            let mut state = self.state.lock().unwrap();
            if !state.initialized {
                return;
            }
            std::mem::take(&mut state.notices)
        };
        for n in notices {
            self.notify_client(&n);
        }
    }

    fn tell_broken(&self) {
        let first = {
            let mut state = self.state.lock().unwrap();
            !std::mem::replace(&mut state.told_broken, true)
        };
        if first {
            log("a message failed in the entry and went through unchanged");
            self.notify_client("mixenc's entry failed on a message and passed it through as it came; answers about CP932 files may be off. Restart the language server.");
        }
    }

    /// The root and relative path (with `/`) of a document, when it is mirrored.
    fn locate(&self, uri: &str) -> Option<(&Root, String, PathBuf)> {
        let path = uri::to_path(uri)?;
        // As the disk spells it, so that the relative path is the one git lists (an index entry
        // per spelling would have the next sync remove the mirror as gone).
        let s = paths::display(&paths::canonical(&path));
        self.roots.iter().find_map(|root| {
            paths::rest_under(&s, &root.source.to_string_lossy())
                .filter(|rest| !rest.is_empty())
                .map(|rest| (root, rest.replace('\\', "/"), path.clone()))
        })
    }

    fn read_as(&self) -> impl Fn(&Path) -> Option<crate::config::ReadAs> + Sync + '_ {
        |abs: &Path| self.ws.override_for(abs).map(|o| o.read_as)
    }

    /// The column map of a document for this client, `None` when its columns need no moving.
    fn map_for(&self, state: &mut State, uri: &str) -> Option<Arc<FileMap>> {
        if let Some(t) = state.translated.get(uri) {
            return Some(t.map.clone());
        }
        let (_, _, path) = self.locate(uri)?;
        let meta = std::fs::metadata(&path).ok()?;
        let (size, mtime) = (meta.len(), crate::fsx::mtime(&meta));
        if let Some((s, m, map)) = state.disk.get(&path)
            && *s == size
            && *m == mtime
        {
            return map.clone();
        }
        let bytes = std::fs::read(&path).ok()?;
        let encoding = self.encoding_of(&path, &bytes);
        let map = FileMap::new(&bytes, encoding, state.units).filter(FileMap::differs).map(Arc::new);
        state.disk.insert(path, (size, mtime, map.clone()));
        map
    }

    fn encoding_of(&self, path: &Path, bytes: &[u8]) -> Encoding {
        match self.ws.override_for(path).map(|o| o.read_as) {
            Some(crate::config::ReadAs::Encoding(e)) => e,
            _ => judge::judge(bytes),
        }
    }

    /// A message from the client, as it goes on to clangd.
    fn client_message(&self, body: &[u8]) -> Vec<u8> {
        let Ok(mut message) = serde_json::from_slice::<Value>(body) else { return body.to_vec() };
        let method = message["method"].as_str().map(str::to_string);
        let changed = match method.as_deref() {
            Some("textDocument/didOpen") => self.did_open(&mut message),
            Some("textDocument/didChange") => self.did_change(&mut message),
            Some("textDocument/didSave") => {
                self.did_save(&message);
                false
            }
            Some("textDocument/didClose") => {
                if let Some(uri) = message["params"]["textDocument"]["uri"].as_str() {
                    self.state.lock().unwrap().translated.remove(uri);
                }
                false
            }
            Some(method) if message.get("id").is_some() => self.request(&mut message, method),
            _ => false,
        };
        if changed { message.to_string().into_bytes() } else { body.to_vec() }
    }

    fn request(&self, message: &mut Value, method: &str) -> bool {
        let id = message["id"].to_string();
        let params = &message["params"];
        let document = params["textDocument"]["uri"]
            .as_str()
            .or_else(|| params["item"]["uri"].as_str())
            .or_else(|| params["uri"].as_str())
            .map(str::to_string);
        let mut state = self.state.lock().unwrap();
        let mut method = method.to_string();
        let mut changed = false;
        if state.misreading
            && let Some(doc) = &document
        {
            if method == "textDocument/semanticTokens/full/delta" && self.map_for(&mut state, doc).is_some() {
                // A full answer is a valid answer to a delta request; its columns can be moved.
                method = "textDocument/semanticTokens/full".into();
                message["method"] = Value::from(method.clone());
                if let Some(p) = message["params"].as_object_mut() {
                    p.remove("previousResultId");
                }
                changed = true;
            }
            let mut lookup = |u: &str| self.map_for(&mut state, u);
            let before = message["params"].clone();
            translate::positions(&mut message["params"], Some(doc), Way::ToMirror, &mut lookup);
            changed |= message["params"] != before;
        }
        state.pending.insert(id, (method, document));
        changed
    }

    fn did_open(&self, message: &mut Value) -> bool {
        let Some(uri) = message["params"]["textDocument"]["uri"].as_str().map(str::to_string) else { return false };
        let Some((root, rel, path)) = self.locate(&uri) else { return false };
        if let Err(err) = mirror::sync_files(root, std::slice::from_ref(&rel), &self.read_as()) {
            log(&format!("mirroring {rel}: {err:#}"));
        }
        let Some(file) = mirror::file_state(root, &rel) else { return false };
        let Some(read_as) = file
            .read_as
            .filter(|e| matches!(e, Encoding::Cp932 | Encoding::Mixed | Encoding::Broken | Encoding::EucJp))
        else {
            return false;
        };
        let (Ok(bytes), Ok(mirror_text)) =
            (std::fs::read(&path), std::fs::read_to_string(paths::join_rel(&root.mirror, &rel)))
        else {
            return false;
        };
        let Some(text) = message["params"]["textDocument"]["text"].as_str() else { return false };
        if text == mirror_text {
            return false;
        }
        let misread = String::from_utf8_lossy(&bytes);
        if text == misread || Some(text) == misread.strip_prefix('\u{FEFF}') {
            let mut state = self.state.lock().unwrap();
            let units = state.units;
            if let Some(map) = FileMap::new(&bytes, read_as, units) {
                if !state.misreading {
                    log(
                        "the client reads CP932 files as UTF-8: its texts are replaced with the mirror's and its columns moved",
                    );
                }
                state.misreading = true;
                state.translated.insert(uri.clone(), Translated { map: Arc::new(map), client_text: text.to_string() });
            }
            message["params"]["textDocument"]["text"] = Value::from(mirror_text);
            return true;
        }
        let tell = {
            let mut state = self.state.lock().unwrap();
            text.contains('\u{FFFD}') && !std::mem::replace(&mut state.told_unknown_text, true)
        };
        if tell {
            self.notify_client(&format!(
                "mixenc: {} came with a text that is neither the original read as UTF-8 nor its mirror; clangd reads it as sent.",
                self.ws.display_path(&path)
            ));
        }
        false
    }

    fn did_change(&self, message: &mut Value) -> bool {
        let Some(uri) = message["params"]["textDocument"]["uri"].as_str().map(str::to_string) else { return false };
        let mut state = self.state.lock().unwrap();
        let units = state.units;
        let Some(doc) = state.translated.get_mut(&uri) else { return false };
        let Some(changes) = message["params"]["contentChanges"].as_array() else { return false };
        for change in changes {
            let Some(new_text) = change["text"].as_str() else { continue };
            match change.get("range") {
                None => doc.client_text = new_text.to_string(),
                Some(range) => apply_edit(&mut doc.client_text, range, new_text, units),
            }
        }
        // Still the original as read from disk: clangd keeps the mirror's text.
        let Some((root, rel, path)) = self.locate(&uri) else { return false };
        let bytes = std::fs::read(&path).unwrap_or_default();
        let text = if doc.client_text == String::from_utf8_lossy(&bytes) {
            std::fs::read_to_string(paths::join_rel(&root.mirror, &rel)).ok()
        } else {
            None
        };
        let text = match text {
            Some(mirror_text) => mirror_text,
            None => {
                // The client edited its misread text: it cannot be read back. clangd gets the
                // client's text, and the columns are the client's again.
                let client_text = doc.client_text.clone();
                state.translated.remove(&uri);
                log(&format!("{rel}: the client edited a misread text; clangd reads it as sent"));
                client_text
            }
        };
        message["params"]["contentChanges"] = json!([{ "text": text }]);
        true
    }

    fn did_save(&self, message: &Value) {
        let Some(uri) = message["params"]["textDocument"]["uri"].as_str() else { return };
        if let Some((root, rel, _)) = self.locate(uri)
            && let Err(err) = mirror::sync_files(root, std::slice::from_ref(&rel), &self.read_as())
        {
            log(&format!("mirroring {rel}: {err:#}"));
        }
    }

    /// A message from clangd, as it goes on to the client.
    fn server_message(&self, body: &[u8]) -> Vec<u8> {
        let Ok(mut message) = serde_json::from_slice::<Value>(body) else { return body.to_vec() };
        let mut state = self.state.lock().unwrap();
        let mut changed = false;
        if message.get("method").is_none() && message.get("id").is_some() {
            let Some((method, document)) = state.pending.remove(&message["id"].to_string()) else {
                return body.to_vec();
            };
            if method == "initialize" {
                let encoding = message["result"]["capabilities"]["positionEncoding"]
                    .as_str()
                    .or_else(|| message["result"]["offsetEncoding"].as_str())
                    .unwrap_or("utf-16");
                state.units = Units::from_name(encoding);
                state.initialized = true;
                return body.to_vec();
            }
            if !state.misreading || message.get("result").is_none_or(Value::is_null) {
                return body.to_vec();
            }
            if method.starts_with("textDocument/semanticTokens/") {
                if let Some(doc) = &document
                    && let Some(map) = self.map_for(&mut state, doc)
                    && let Some(data) = message["result"]["data"].as_array_mut()
                {
                    translate::semantic_tokens(data, &map, Way::ToClient);
                    changed = true;
                }
            } else {
                let before = message["result"].clone();
                let mut lookup = |u: &str| self.map_for(&mut state, u);
                translate::positions(&mut message["result"], document.as_deref(), Way::ToClient, &mut lookup);
                changed = message["result"] != before;
            }
        } else if state.misreading && message.get("params").is_some() {
            let document = message["params"]["uri"].as_str().map(str::to_string);
            let before = message["params"].clone();
            let mut lookup = |u: &str| self.map_for(&mut state, u);
            translate::positions(&mut message["params"], document.as_deref(), Way::ToClient, &mut lookup);
            changed = message["params"] != before;
        }
        if changed { message.to_string().into_bytes() } else { body.to_vec() }
    }

    /// Watches the repositories and the databases; syncs after a change has settled.
    fn watch(self: Arc<Self>) {
        use notify::{RecursiveMode, Watcher};
        let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
        let Ok(mut watcher) = notify::recommended_watcher(tx) else {
            log("cannot watch the repositories; changes outside the editor reach the mirror at the next start");
            return;
        };
        for repo in &self.repos {
            if let Err(err) = watcher.watch(&repo.source, RecursiveMode::Recursive) {
                log(&format!("cannot watch {}: {err}", paths::display(&repo.source)));
            }
            if let Some(state) = sync::RepoState::load(&repo.mirror)
                && let Some(db) = state.db
                && let Some(parent) = Path::new(&db.source).parent()
                && !paths::is_under(&parent.to_string_lossy(), &repo.source.to_string_lossy())
            {
                let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
            }
        }
        loop {
            // Wait for a change that matters, then for quiet.
            let Ok(event) = rx.recv() else { return };
            let mut relevant = matters(&event);
            loop {
                match rx.recv_timeout(Duration::from_millis(800)) {
                    Ok(event) => relevant |= matters(&event),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(_) => return,
                }
            }
            if !relevant {
                continue;
            }
            for repo in &self.repos {
                match sync::sync_repo(&self.ws, repo) {
                    Ok(result) => {
                        let notices = problems_of(&result);
                        self.state.lock().unwrap().notices.extend(notices);
                    }
                    Err(err) => log(&format!("syncing {}: {err:#}", repo.name)),
                }
            }
            self.flush_notices();
        }
    }
}

/// Whether a change in a repository can matter to the mirror (not git's own files, not build
/// output).
fn matters(event: &notify::Result<notify::Event>) -> bool {
    let Ok(event) = event else { return true };
    event.paths.iter().any(|p| {
        let s = p.to_string_lossy().to_lowercase().replace('/', "\\");
        let ignored_dir = ["\\.git\\", "\\.vs\\", "\\.cache\\"].iter().any(|d| s.contains(d)) || s.ends_with("\\.git");
        let ext = s.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
        let build_output = matches!(
            ext,
            "obj"
                | "pdb"
                | "ilk"
                | "exe"
                | "dll"
                | "lib"
                | "exp"
                | "pch"
                | "ipch"
                | "idb"
                | "iobj"
                | "ipdb"
                | "tlog"
                | "log"
                | "res"
                | "lastbuildstate"
                | "recipe"
                | "db"
                | "opendb"
                | "suo"
                | "cache"
                | "tmp"
        );
        !ignored_dir && !build_output
    })
}

/// What the client should know after a sync.
fn problems_of(result: &sync::RepoSync) -> Vec<String> {
    let mut undecided = 0;
    let mut failed = 0;
    for outcome in std::iter::once(&result.repo).chain(result.external.iter().map(|(_, o)| o)) {
        for (_, problem) in &outcome.not_mirrored {
            match problem {
                Problem::Undecided => undecided += 1,
                Problem::Skipped => {}
                _ => failed += 1,
            }
        }
    }
    let mut out = Vec::new();
    if failed > 0 || !result.state.db_problems.is_empty() {
        out.push(format!(
            "mixenc ({}): {failed} files could not be mirrored, {} problems with the database. Run `mixenc doctor`.",
            result.name,
            result.state.db_problems.len()
        ));
    }
    if undecided > 0 {
        out.push(format!(
            "mixenc ({}): {undecided} files wait for an encoding to be decided (or are binary). `mixenc doctor` lists them.",
            result.name
        ));
    }
    out
}

/// Applies an LSP edit (`range` in `units`) to `text`.
fn apply_edit(text: &mut String, range: &Value, new_text: &str, units: Units) {
    let offset = |position: &Value| -> Option<usize> {
        let line = position["line"].as_u64()? as usize;
        let character = position["character"].as_u64()? as usize;
        let mut start = 0;
        for _ in 0..line {
            let rest = &text[start..];
            let next = rest.find(['\n', '\r'])?;
            start += next + if rest[next..].starts_with("\r\n") { 2 } else { 1 };
        }
        let mut column = 0;
        for (i, c) in text[start..].char_indices() {
            if column >= character || c == '\n' || c == '\r' {
                return Some(start + i);
            }
            column += match units {
                Units::Utf8 => c.len_utf8(),
                Units::Utf16 => c.len_utf16(),
                Units::Utf32 => 1,
            };
        }
        Some(text.len())
    };
    if let (Some(a), Some(b)) = (offset(&range["start"]), offset(&range["end"]))
        && a <= b
    {
        text.replace_range(a..b, new_text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_in_utf16_columns() {
        let mut text = "a表b\r\nxyz\n".to_string();
        let range = json!({"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 2}});
        apply_edit(&mut text, &range, "示", Units::Utf16);
        assert_eq!(text, "a示b\r\nxyz\n");
        let range = json!({"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 3}});
        apply_edit(&mut text, &range, "!", Units::Utf16);
        assert_eq!(text, "a示b\r\nxyz!\n");
    }

    #[test]
    fn build_output_does_not_matter() {
        let event = |p: &str| Ok(notify::Event::new(notify::EventKind::Any).add_path(PathBuf::from(p)));
        assert!(matters(&event(r"C:\r\src\a.cpp")));
        assert!(!matters(&event(r"C:\r\x64\Debug\a.obj")));
        assert!(!matters(&event(r"C:\r\.git\index")));
        assert!(!matters(&event(r"C:\r\.vs\r\v17\ipch\x.ipch")));
    }
}
