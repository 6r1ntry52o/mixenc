//! A language server client for the analysis tools. It starts the entry (`mixenc clangd`, this same
//! program) on a workspace and asks clangd through it: the entry mirrors, maps the paths and
//! watches the repositories, and the client opens files with their mirror's text (read right)
//! under the original's URI, so that every answer names the originals.

use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::{lsp, paths, uri};

/// Files kept open in clangd at most; the least recently used is closed.
const OPEN_LIMIT: usize = 12;

type Reply = std::result::Result<Value, Value>;

/// What clangd said on its own: the background index's progress, diagnostics, messages.
#[derive(Default, Debug)]
pub struct Heard {
    /// The background index: `Some(percentage)` while it runs.
    pub indexing: Option<u64>,
    pub index_message: String,
    /// The background index ran to its end at least once.
    pub indexed: bool,
    /// Diagnostics by the file's path key.
    pub diagnostics: HashMap<String, Vec<Value>>,
    /// `window/showMessage` from the entry or clangd (the newest last).
    pub messages: Vec<String>,
    pub alive: bool,
}

struct Open {
    uri: String,
    version: i64,
    text: String,
    used: Instant,
}

pub struct Client {
    to_server: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<i64, Sender<Reply>>>>,
    next_id: AtomicI64,
    child: Mutex<Child>,
    pub heard: Arc<Mutex<Heard>>,
    open: Mutex<HashMap<String, Open>>,
}

impl Client {
    /// Starts the entry with clangd for `folders` (the repositories of one workspace).
    pub fn start(folders: &[PathBuf]) -> Result<Client> {
        let me = std::env::current_exe().context("finding mixenc itself")?;
        let mut child = Command::new(&me)
            .args(["clangd", "--background-index", "--limit-references=0", "--limit-results=500"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting {} clangd", paths::display(&me)))?;
        let to_server = child.stdin.take().context("the entry's stdin")?;
        let from_server = child.stdout.take().context("the entry's stdout")?;
        let client = Client {
            to_server: Arc::new(Mutex::new(to_server)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicI64::new(1),
            child: Mutex::new(child),
            heard: Arc::new(Mutex::new(Heard { alive: true, ..Heard::default() })),
            open: Mutex::new(HashMap::new()),
        };
        client.listen(from_server, folders.to_vec());

        let workspace_folders: Vec<Value> = folders
            .iter()
            .map(|f| {
                let name = f.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                json!({ "uri": uri::from_path(f), "name": name })
            })
            .collect();
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "mixenc-mcp", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": workspace_folders.first().map(|f| f["uri"].clone()),
            "workspaceFolders": workspace_folders,
            "capabilities": {
                "general": { "positionEncodings": ["utf-16"] },
                "window": { "workDoneProgress": true },
                "workspace": { "symbol": {}, "workspaceFolders": true, "configuration": true },
                "textDocument": {
                    "synchronization": { "didSave": true },
                    "definition": { "linkSupport": true },
                    "declaration": { "linkSupport": true },
                    "references": {},
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "callHierarchy": {},
                    "typeHierarchy": {},
                    "publishDiagnostics": {},
                },
            },
        });
        client.request("initialize", params, Duration::from_secs(120)).context("clangd did not start")?;
        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    fn listen(&self, from_server: std::process::ChildStdout, folders: Vec<PathBuf>) {
        let pending = Arc::clone(&self.pending);
        let heard = Arc::clone(&self.heard);
        let writer = Arc::clone(&self.to_server);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(from_server);
            while let Ok(Some(body)) = lsp::read(&mut reader) {
                let Ok(message) = serde_json::from_slice::<Value>(&body) else { continue };
                let method = message.get("method").and_then(Value::as_str);
                match (method, message.get("id")) {
                    (None, Some(id)) => {
                        let Some(id) = id.as_i64() else { continue };
                        if let Some(tx) = pending.lock().unwrap().remove(&id) {
                            let reply = match message.get("error") {
                                Some(err) => Err(err.clone()),
                                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                            };
                            let _ = tx.send(reply);
                        }
                    }
                    (Some(method), Some(id)) => {
                        let result = match method {
                            "workspace/configuration" => {
                                Value::Array(vec![
                                    Value::Null;
                                    message["params"]["items"].as_array().map_or(0, Vec::len)
                                ])
                            }
                            "workspace/workspaceFolders" => Value::Array(
                                folders.iter().map(|f| json!({ "uri": uri::from_path(f), "name": "" })).collect(),
                            ),
                            _ => Value::Null,
                        };
                        let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                        let _ = lsp::write(&mut *writer.lock().unwrap(), reply.to_string().as_bytes());
                    }
                    (Some(method), None) => hear(&heard, method, &message["params"]),
                    (None, None) => {}
                }
            }
            heard.lock().unwrap().alive = false;
            for (_, tx) in pending.lock().unwrap().drain() {
                let _ = tx.send(Err(json!({ "message": "clangd stopped" })));
            }
        });
    }

    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        match rx.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => {
                bail!("clangd answered {method} with an error: {}", err["message"].as_str().unwrap_or("(no message)"))
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                let _ = self.notify("$/cancelRequest", json!({ "id": id }));
                bail!("clangd did not answer {method} within {} s", timeout.as_secs())
            }
        }
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn send(&self, message: &Value) -> Result<()> {
        if !self.heard.lock().unwrap().alive {
            bail!("clangd is not running (it stopped; the next question starts it again)");
        }
        let mut w = self.to_server.lock().unwrap();
        lsp::write(&mut *w, message.to_string().as_bytes()).map_err(|e| anyhow!("writing to clangd: {e}"))
    }

    pub fn is_alive(&self) -> bool {
        self.heard.lock().unwrap().alive
    }

    /// Opens a file in clangd with `text` (or brings its text up to date) under its original path.
    pub fn open(&self, original: &Path, text: &str) -> Result<String> {
        let key = paths::key(&original.to_string_lossy());
        let mut open = self.open.lock().unwrap();
        if let Some(doc) = open.get_mut(&key) {
            doc.used = Instant::now();
            if doc.text != text {
                doc.version += 1;
                doc.text = text.to_string();
                let params = json!({
                    "textDocument": { "uri": doc.uri, "version": doc.version },
                    "contentChanges": [{ "text": text }],
                });
                self.notify("textDocument/didChange", params)?;
            }
            return Ok(doc.uri.clone());
        }
        if open.len() >= OPEN_LIMIT
            && let Some(oldest) = open.iter().min_by_key(|(_, d)| d.used).map(|(k, _)| k.clone())
            && let Some(doc) = open.remove(&oldest)
        {
            self.notify("textDocument/didClose", json!({ "textDocument": { "uri": doc.uri } }))?;
        }
        let uri = uri::from_path(original);
        let language = match original.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("c") => "c",
            _ => "cpp",
        };
        let params = json!({ "textDocument": { "uri": uri, "languageId": language, "version": 1, "text": text } });
        self.notify("textDocument/didOpen", params)?;
        open.insert(key, Open { uri: uri.clone(), version: 1, text: text.to_string(), used: Instant::now() });
        Ok(uri)
    }

    /// Waits up to `timeout` while the background index runs; says how it stands.
    pub fn wait_index(&self, timeout: Duration) -> IndexState {
        let until = Instant::now() + timeout;
        loop {
            let state = self.index_state();
            if !matches!(state, IndexState::Running(..) | IndexState::NotStarted) || Instant::now() >= until {
                return state;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn index_state(&self) -> IndexState {
        let heard = self.heard.lock().unwrap();
        match (heard.indexing, heard.indexed) {
            (Some(p), _) => IndexState::Running(p, heard.index_message.clone()),
            (None, true) => IndexState::Done,
            (None, false) => IndexState::NotStarted,
        }
    }

    /// The diagnostics clangd last published for a file.
    pub fn diagnostics(&self, original: &Path) -> Option<Vec<Value>> {
        self.heard.lock().unwrap().diagnostics.get(&paths::key(&original.to_string_lossy())).cloned()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexState {
    NotStarted,
    Running(u64, String),
    Done,
}

impl IndexState {
    /// A note for an answer that comes from the index, when the index is not complete.
    pub fn note(&self) -> Option<String> {
        match self {
            IndexState::Done => None,
            IndexState::Running(p, message) => Some(format!(
                "clangd is still indexing ({p}%{}): files not indexed yet are missing from this answer",
                if message.is_empty() { String::new() } else { format!(", {message}") }
            )),
            IndexState::NotStarted => {
                Some("clangd's background index has not run yet: only files opened so far are in this answer".into())
            }
        }
    }
}

fn hear(heard: &Mutex<Heard>, method: &str, params: &Value) {
    let mut heard = heard.lock().unwrap();
    match method {
        "$/progress" => {
            let token = params["token"].as_str().unwrap_or_default();
            let value = &params["value"];
            let is_index = token.contains("backgroundIndex")
                || value["title"].as_str().is_some_and(|t| t.to_ascii_lowercase().contains("index"));
            if !is_index && heard.indexing.is_none() {
                return;
            }
            match value["kind"].as_str() {
                Some("begin") if is_index => {
                    heard.indexing = Some(value["percentage"].as_u64().unwrap_or(0));
                    heard.index_message = value["message"].as_str().unwrap_or_default().to_string();
                }
                Some("report") if heard.indexing.is_some() => {
                    if let Some(p) = value["percentage"].as_u64() {
                        heard.indexing = Some(p);
                    }
                    if let Some(m) = value["message"].as_str() {
                        heard.index_message = m.to_string();
                    }
                }
                Some("end") if is_index => {
                    heard.indexing = None;
                    heard.indexed = true;
                    heard.index_message.clear();
                }
                _ => {}
            }
        }
        "textDocument/publishDiagnostics" => {
            if let Some(path) = params["uri"].as_str().and_then(uri::to_path) {
                let list = params["diagnostics"].as_array().cloned().unwrap_or_default();
                heard.diagnostics.insert(paths::key(&path.to_string_lossy()), list);
            }
        }
        "window/showMessage" => {
            if let Some(m) = params["message"].as_str() {
                heard.messages.push(m.to_string());
                if heard.messages.len() > 20 {
                    heard.messages.remove(0);
                }
            }
        }
        _ => {}
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.request("shutdown", Value::Null, Duration::from_secs(3));
        let _ = self.notify("exit", Value::Null);
        let mut child = self.child.lock().unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
    }
}
