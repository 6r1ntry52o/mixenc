//! The entry end to end, with a real clangd: a client that reads the CP932 originals right (as VS
//! Code does) and one that sends them misread as UTF-8 (as agents' LSP clients do). Needs clangd:
//! `MIXENC_TEST_CLANGD`, LLVM's or one on PATH; without one the tests say so and pass, unless
//! `MIXENC_REQUIRE_CLANGD` is set (CI).

use std::fs;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

fn find_clangd() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MIXENC_TEST_CLANGD") {
        return Some(PathBuf::from(p));
    }
    let mut candidates = vec![PathBuf::from(r"C:\Program Files\LLVM\bin\clangd.exe")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let mason = PathBuf::from(local).join(r"nvim-data\mason\packages\clangd");
        for version in fs::read_dir(mason).into_iter().flatten().flatten() {
            candidates.push(version.path().join(r"bin\clangd.exe"));
        }
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        candidates.push(dir.join("clangd.exe"));
    }
    let found = candidates.into_iter().find(|p| p.is_file());
    if found.is_none() {
        assert!(std::env::var_os("MIXENC_REQUIRE_CLANGD").is_none(), "clangd is required here and was not found");
        eprintln!("no clangd: the entry's end-to-end tests are skipped");
    }
    found
}

fn sjis(s: &str) -> Vec<u8> {
    encoding_rs::SHIFT_JIS.encode(s).0.into_owned()
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Line 4 of a.cpp: a CP932 string before the calls, so its columns differ between the readings.
const A: &str = "#include \"hyo.h\"\n#include \"ext_hidden.h\"\n// 能\nint only_b();\nint main() { const char* s = \"表示\"; return hidden(1) + only_b() + ext_hidden(); }\n";
/// Line 2 of hyo.h: CP932 before the declaration, likewise.
const HYO: &str = "#pragma once\n// 表\n/* 表示 */ int hidden(int);\n";

struct Fixture {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    repo: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = mixenc::paths::canonical(tmp.path());
    let ws = base.join("ws");
    let repo = ws.join("repos").join("cloneA");
    let ext = base.join("vendor").join("inc");
    let crlf = |s: &str| s.replace('\n', "\r\n");
    write(&repo.join("src/hyo.h"), &sjis(&crlf(HYO)));
    write(&repo.join("src/a.cpp"), &sjis(&crlf(A)));
    write(&repo.join("src/b.cpp"), &sjis(&crlf("// ソ\nint only_b() { return 2; }\n")));
    write(&ext.join("ext_hidden.h"), &sjis(&crlf("#pragma once\n// 表\nint ext_hidden();\n")));
    // EUC-JP that CP932 reads too: it waits for a person, and the entry says so.
    write(&repo.join("docs/euc_or_cp932.txt"), &encoding_rs::EUC_JP.encode("アイウ\n").0);
    let src = repo.join("src");
    let db: Vec<Value> = ["a.cpp", "b.cpp"]
        .iter()
        .map(|f| {
            json!({
                "directory": src.display().to_string(),
                "file": src.join(f).display().to_string(),
                "arguments": ["cl.exe", "/c", format!("/I{}", src.display()), "/I", ext.display().to_string(), "/source-charset:shift_jis", f],
            })
        })
        .collect();
    write(&repo.join("compile_commands.json"), serde_json::to_string(&db).unwrap().as_bytes());
    write(&repo.join(".gitignore"), b"compile_commands.json\n");
    for args in [&["init", "-q"][..], &["add", "-A"]] {
        assert!(Command::new("git").args(args).current_dir(&repo).status().unwrap().success());
    }
    write(
        &ws.join("mixenc.json"),
        br#"{"repos": [{"path": "repos/cloneA", "db": "repos/cloneA/compile_commands.json"}]}"#,
    );
    Fixture { _tmp: tmp, ws, repo }
}

/// A file URI as VS Code writes it (lower-case drive, `%3A`).
fn uri(path: &Path) -> String {
    let s = path.display().to_string().replace('\\', "/");
    let encoded: String = s[2..]
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("file:///{}%3A{encoded}", s[..1].to_lowercase())
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    next: i64,
    diagnostics: Vec<(String, Vec<String>)>,
    shown: Vec<String>,
}

impl Client {
    fn start(f: &Fixture, clangd: &Path) -> Client {
        Client::start_at(f, clangd, &f.repo)
    }

    fn start_at(f: &Fixture, clangd: &Path, folder: &Path) -> Client {
        Client::start_with(f, clangd, &[folder], &[])
    }

    /// The entry for these folders, with more arguments (`--mixenc-config <file>`).
    fn start_with(f: &Fixture, clangd: &Path, folders: &[&Path], extra: &[String]) -> Client {
        let folder = folders[0];
        let log = fs::File::create(f.ws.join(format!(
            "entry-{}.log",
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
        )))
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_mixenc"))
            .args(["clangd", "--background-index", "--log=verbose"])
            .args(extra)
            .env("MIXENC_CLANGD", clangd)
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(body)) = mixenc::lsp::read(&mut reader) {
                if tx.send(serde_json::from_slice::<Value>(&body).unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut client = Client { child, stdin, messages: rx, next: 0, diagnostics: Vec::new(), shown: Vec::new() };
        let root = uri(folder);
        let workspace_folders: Vec<Value> = folders.iter().map(|d| json!({"uri": uri(d), "name": "folder"})).collect();
        client.request(
            "initialize",
            json!({"processId": null, "rootUri": root, "workspaceFolders": workspace_folders, "capabilities": {}}),
        );
        client.notify("initialized", json!({}));
        client
    }

    fn send(&mut self, message: Value) {
        mixenc::lsp::write(&mut self.stdin, message.to_string().as_bytes()).unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn handle(&mut self, m: &Value) {
        if m["method"] == "window/showMessage" {
            self.shown.push(m["params"]["message"].as_str().unwrap_or_default().to_string());
        } else if m["method"] == "textDocument/publishDiagnostics" {
            let messages = m["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d["message"].as_str().unwrap().to_string())
                .collect();
            self.diagnostics.push((m["params"]["uri"].as_str().unwrap().to_string(), messages));
        } else if m.get("method").is_some() && m.get("id").is_some() {
            let id = m["id"].clone();
            self.send(json!({"jsonrpc": "2.0", "id": id, "result": null}));
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            let Ok(m) = self.messages.recv_timeout(Duration::from_millis(200)) else { continue };
            self.handle(&m);
            if m["id"] == id && m.get("method").is_none() {
                return m["result"].clone();
            }
        }
        panic!("no answer to {method}");
    }

    /// Waits for diagnostics of `doc` and returns them (URIs compared as paths: clangd writes
    /// `file:///C:/...`, VS Code `file:///c%3A/...`).
    fn diagnostics_of(&mut self, doc: &str) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some((_, d)) = self.diagnostics.iter().rev().find(|(u, _)| same_document(u, doc)) {
                return d.clone();
            }
            assert!(Instant::now() < deadline, "no diagnostics for {doc}; got {:?}", self.diagnostics);
            if let Ok(m) = self.messages.recv_timeout(Duration::from_millis(200)) {
                self.handle(&m);
            }
        }
    }

    fn open(&mut self, doc: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": doc, "languageId": "cpp", "version": 1, "text": text}}),
        );
    }

    /// `textDocument/definition` until it answers (the index may still be building).
    fn definition(&mut self, doc: &str, line: usize, character: usize) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let r = self.request(
                "textDocument/definition",
                json!({"textDocument": {"uri": doc}, "position": {"line": line, "character": character}}),
            );
            let found = r.as_array().cloned().unwrap_or_default();
            if !found.is_empty() || Instant::now() > deadline {
                return found;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// `textDocument/definition` until a location `wanted` takes is among the answers: clangd
    /// answers with the declaration in the open file before its background index has the
    /// definition in a file that is not open.
    fn definition_until(
        &mut self,
        doc: &str,
        line: usize,
        character: usize,
        wanted: impl Fn(&Value) -> bool,
    ) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let found = self.definition(doc, line, character);
            if found.iter().any(&wanted) || Instant::now() > deadline {
                return found;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn stop(mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        drop(self.stdin);
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        panic!("the entry did not end after exit");
    }
}

/// The column of `needle` in `line` of `text`, in UTF-16 units.
fn column(text: &str, line: usize, needle: &str) -> usize {
    let l = text.lines().nth(line).unwrap();
    l[..l.find(needle).unwrap()].encode_utf16().count()
}

fn same_document(a: &str, b: &str) -> bool {
    let key = |u: &str| mixenc::uri::to_path(u).map(|p| mixenc::paths::key(&p.to_string_lossy()));
    key(a).is_some() && key(a) == key(b)
}

fn in_original(location: &Value, name: &str) -> bool {
    let u = location["uri"].as_str().or(location["targetUri"].as_str()).unwrap();
    let decoded = mixenc::uri::to_path(u).unwrap().display().to_string().to_lowercase();
    decoded.ends_with(name) && !decoded.contains("\\mirror\\")
}

#[test]
fn a_client_that_reads_cp932_right() {
    let Some(clangd) = find_clangd() else { return };
    let f = fixture();
    let mut c = Client::start(&f, &clangd);
    let a = uri(&f.repo.join("src/a.cpp"));
    let text = A.replace('\n', "\r\n");
    c.open(&a, &text);
    assert_eq!(c.diagnostics_of(&a), Vec::<String>::new(), "the mirror is read, so // 能 swallows nothing");

    let hidden = c.definition(&a, 4, column(&text, 4, "hidden"));
    assert!(in_original(&hidden[0], "hyo.h"), "{hidden:?}");
    assert_eq!(hidden[0]["range"]["start"]["line"], 2);
    let ext = c.definition(&a, 4, column(&text, 4, "ext_hidden"));
    assert!(in_original(&ext[0], "ext_hidden.h"), "{ext:?}");
    // only_b is defined in b.cpp, which is not open: the background index read its mirror.
    let only_b = c.definition_until(&a, 4, column(&text, 4, "only_b"), |l| in_original(l, "b.cpp"));
    assert!(only_b.iter().any(|l| in_original(l, "b.cpp")), "{only_b:?}");
    c.stop();
}

#[test]
fn a_client_that_sends_cp932_misread_as_utf8() {
    let Some(clangd) = find_clangd() else { return };
    let f = fixture();
    let mut c = Client::start(&f, &clangd);
    let a = uri(&f.repo.join("src/a.cpp"));
    let misread = String::from_utf8_lossy(&fs::read(f.repo.join("src/a.cpp")).unwrap()).into_owned();
    c.open(&a, &misread);
    assert_eq!(c.diagnostics_of(&a), Vec::<String>::new(), "the misread text was replaced with the mirror's");

    // Columns in the client's reading: "表示" is U+FFFD and `\` and more to it.
    let col = column(&misread, 4, "hidden");
    assert_ne!(col, column(A, 4, "hidden"));
    let hidden = c.definition(&a, 4, col);
    assert!(in_original(&hidden[0], "hyo.h"), "{hidden:?}");
    let highlights = c.request(
        "textDocument/documentHighlight",
        json!({"textDocument": {"uri": a}, "position": {"line": 4, "character": col}}),
    );
    let on_line_4: Vec<&Value> =
        highlights.as_array().unwrap().iter().filter(|h| h["range"]["start"]["line"] == 4).collect();
    assert_eq!(
        on_line_4[0]["range"]["start"]["character"], col,
        "the answer comes back in the client's columns: {highlights}"
    );
    assert_eq!(on_line_4[0]["range"]["end"]["character"], col + "hidden".len());

    // The same client opens hyo.h read right (as VS Code does once the encoding is set): the
    // columns of that document are not moved.
    let h = uri(&f.repo.join("src/hyo.h"));
    let right = HYO.replace('\n', "\r\n");
    c.open(&h, &right);
    c.diagnostics_of(&h);
    let col = column(&right, 2, "hidden");
    let highlights = c.request(
        "textDocument/documentHighlight",
        json!({"textDocument": {"uri": h}, "position": {"line": 2, "character": col}}),
    );
    assert_eq!(highlights[0]["range"]["start"]["character"], col, "{highlights}");
    c.stop();
}

#[test]
fn a_second_start_does_not_index_again() {
    let Some(clangd) = find_clangd() else { return };
    let f = fixture();
    let index = f.ws.join(r"mirror\cloneA\.cache\clangd\index");
    let a = uri(&f.repo.join("src/a.cpp"));
    let text = A.replace('\n', "\r\n");

    let mut c = Client::start(&f, &clangd);
    c.open(&a, &text);
    c.diagnostics_of(&a);
    let deadline = Instant::now() + Duration::from_secs(60);
    let shards = loop {
        let shards: Vec<(PathBuf, SystemTime)> = fs::read_dir(&index)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| (e.path(), e.metadata().unwrap().modified().unwrap()))
            // The shards; clangd writes its .gitignore there at every start.
            .filter(|(p, _)| p.extension().is_some_and(|e| e == "idx"))
            .collect();
        if shards.iter().filter(|(p, _)| p.to_string_lossy().contains("b.cpp")).count() > 0 && shards.len() >= 4 {
            break shards;
        }
        assert!(Instant::now() < deadline, "clangd did not save its index in {}", index.display());
        std::thread::sleep(Duration::from_millis(300));
    };
    c.stop();

    let mut c = Client::start(&f, &clangd);
    c.open(&a, &text);
    c.diagnostics_of(&a);
    std::thread::sleep(Duration::from_secs(3));
    c.stop();
    for (shard, modified) in shards {
        assert_eq!(
            fs::metadata(&shard).unwrap().modified().unwrap(),
            modified,
            "{} was written again",
            shard.display()
        );
    }
    let _ = std::io::stdout().flush();
}

#[test]
fn it_says_what_waits_and_follows_changes_outside_the_editor() {
    let Some(clangd) = find_clangd() else { return };
    let f = fixture();
    let mut c = Client::start(&f, &clangd);
    let a = uri(&f.repo.join("src/a.cpp"));
    c.open(&a, &A.replace('\n', "\r\n"));
    c.diagnostics_of(&a);
    assert!(c.shown.iter().any(|m| m.contains("wait for an encoding")), "{:?}", c.shown);

    // A change made outside the editor (a checkout, another tool) reaches the mirror.
    write(&f.repo.join("src/hyo.h"), &sjis("#pragma once\r\n// 表\r\nint hidden(int);\r\nint hidden2(int);\r\n"));
    let mirror = f.ws.join(r"mirror\cloneA\src\hyo.h");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !fs::read_to_string(&mirror).unwrap().contains("hidden2") {
        assert!(Instant::now() < deadline, "the change did not reach the mirror");
        std::thread::sleep(Duration::from_millis(200));
    }
    c.stop();
}

#[test]
fn without_a_workspace_it_passes_through() {
    let Some(clangd) = find_clangd() else { return };
    let f = fixture();
    let elsewhere = f.ws.parent().unwrap().join("vendor");
    let c = Client::start_at(&f, &clangd, &elsewhere);
    c.stop();

    let out = Command::new(env!("CARGO_BIN_EXE_mixenc"))
        .args(["clangd", "--version"])
        .env("MIXENC_CLANGD", &clangd)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("clangd version"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Stage 6: two repositories with a settings file each, kept in a vault in git, their mirrors
/// outside it; one entry (an editor has one clangd for all its folders) serves both.
#[test]
fn one_entry_serves_the_settings_files_of_a_window() {
    let Some(clangd) = find_clangd() else { return };
    let f = fixture();
    let base = f.ws.parent().unwrap().to_path_buf();
    let repo_b = base.join("src").join("cloneB");
    let crlf = |s: &str| s.replace('\n', "\r\n");
    write(&repo_b.join("lib/sho.h"), &sjis(&crlf("#pragma once\n// 表\n/* 能 */ int shown_b(int);\n")));
    write(
        &repo_b.join("lib/use.cpp"),
        &sjis(&crlf("#include \"sho.h\"\n// ソ\nint use() { const char* s = \"表示\"; return shown_b(2); }\n")),
    );
    let lib = repo_b.join("lib");
    let db = json!([{
        "directory": lib.display().to_string(),
        "file": lib.join("use.cpp").display().to_string(),
        "arguments": ["cl.exe", "/c", format!("/I{}", lib.display()), "/source-charset:shift_jis", "use.cpp"],
    }]);
    write(&repo_b.join("compile_commands.json"), db.to_string().as_bytes());
    write(&repo_b.join(".gitignore"), b"compile_commands.json\n");
    for args in [&["init", "-q"][..], &["add", "-A"]] {
        assert!(Command::new("git").args(args).current_dir(&repo_b).status().unwrap().success());
    }
    let vault = base.join("vault");
    fs::create_dir_all(&vault).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&vault).status().unwrap().success());
    write(
        &vault.join("cloneA.mixenc.json"),
        br#"{"mirror": "../mirror", "repos": [{"path": "../ws/repos/cloneA", "db": "../ws/repos/cloneA/compile_commands.json"}]}"#,
    );
    write(
        &vault.join("cloneB.mixenc.json"),
        br#"{"mirror": "../mirror", "repos": [{"path": "../src/cloneB", "db": "../src/cloneB/compile_commands.json"}]}"#,
    );
    let configs: Vec<String> = ["cloneA", "cloneB"]
        .iter()
        .flat_map(|n| ["--mixenc-config".to_string(), vault.join(format!("{n}.mixenc.json")).display().to_string()])
        .collect();
    let mut c = Client::start_with(&f, &clangd, &[&vault, &f.repo, &repo_b], &configs);

    let a = uri(&f.repo.join("src/a.cpp"));
    let text_a = A.replace('\n', "\r\n");
    c.open(&a, &text_a);
    assert_eq!(c.diagnostics_of(&a), Vec::<String>::new());
    let hidden = c.definition(&a, 4, column(&text_a, 4, "hidden"));
    assert!(in_original(&hidden[0], "hyo.h"), "{hidden:?}");

    let u = uri(&repo_b.join("lib/use.cpp"));
    let text_b = crlf("#include \"sho.h\"\n// ソ\nint use() { const char* s = \"表示\"; return shown_b(2); }\n");
    c.open(&u, &text_b);
    assert_eq!(c.diagnostics_of(&u), Vec::<String>::new(), "cloneB is read from its mirror too");
    let shown = c.definition(&u, 2, column(&text_b, 2, "shown_b"));
    assert!(in_original(&shown[0], "sho.h"), "{shown:?}");
    assert_eq!(shown[0]["range"]["start"]["line"], 2);
    c.stop();

    // Both mirrors are outside the vault, and git there sees only the settings files.
    assert!(base.join(r"mirror\cloneA\src\a.cpp").is_file() && base.join(r"mirror\cloneB\lib\use.cpp").is_file());
    let status = Command::new("git").args(["status", "--porcelain", "-uall"]).current_dir(&vault).output().unwrap();
    let status = String::from_utf8(status.stdout).unwrap();
    assert_eq!(status.lines().count(), 2, "{status}");
    assert!(status.lines().all(|l| l.ends_with(".mixenc.json")), "{status}");
}
