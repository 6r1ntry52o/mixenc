//! The analysis tools end to end: `mixenc mcp` starts the entry and a real clangd on the sample, and
//! every answer names the originals and reads CP932 right. Needs clangd (see `common::find_clangd`).

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use common::Sample;
use serde_json::{Value, json};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: i64,
}

impl Mcp {
    fn start(s: &Sample, clangd: &Path) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mixenc"))
            .arg("mcp")
            .current_dir(&s.repo)
            .env("MIXENC_CLANGD", clangd)
            .env("COPILOT_HOME", &s.copilot_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Mcp { child, stdin, stdout, next: 1 };
        let init = mcp.ask("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {} }));
        assert_eq!(init["result"]["serverInfo"]["name"], "mixenc");
        mcp
    }

    fn ask(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        writeln!(self.stdin, "{}", json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// A tool's text; panics on a refusal.
    fn tool(&mut self, name: &str, args: Value) -> String {
        let reply = self.ask("tools/call", json!({ "name": name, "arguments": args }));
        let text = reply["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string();
        assert_eq!(reply["result"]["isError"], false, "{name} {args}: {reply}");
        assert!(!text.contains("mirror/") && !text.contains("mirror\\"), "{name} answered a mirror path: {text}");
        text
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_analysis_tools_answer_from_a_symbol_to_its_call_tree_with_the_originals() {
    let Some(clangd) = common::find_clangd() else { return };
    let s = Sample::new();
    let mut mcp = Mcp::start(&s, &clangd);

    // ① symbols, from clangd's index (the tool waits for it).
    let found = mcp.tool("find_symbol", json!({ "query": "Init" }));
    assert!(found.contains("method Engine::Init  src/engine.cpp:5"), "{found}");
    assert!(found.contains("method Base::Init  src/engine.cpp:4"), "{found}");

    // ② the definition apart from the declaration, the lines read right.
    let def = mcp.tool("definition", json!({ "symbol": "Engine::Init" }));
    assert!(def.contains("definition of Engine::Init: src/engine.cpp:5"), "{def}");
    assert!(def.contains("declaration of Engine::Init: src/engine.h:10"), "{def}");
    assert!(def.contains(">    5  int Engine::Init() { const char* s = \"表示\"; return Helper(); }"), "{def}");
    let from_main = mcp.tool("definition", json!({ "path": "src/main.cpp", "line": 3, "symbol": "Run" }));
    assert!(from_main.contains("definition of Run: src/engine.cpp:6"), "{from_main}");

    let callers = mcp.tool("call_hierarchy", json!({ "symbol": "Engine::Init", "depth": 3 }));
    assert!(callers.contains("Engine::Init  src/engine.cpp:5"), "{callers}");
    assert!(callers.contains("  <- Engine::Run  src/engine.cpp:6  (calls at 6)"), "{callers}");
    assert!(callers.contains("    <- main  src/main.cpp:3  (calls at 3)"), "{callers}");
    let callees = mcp.tool("call_hierarchy", json!({ "symbol": "Engine::Init", "direction": "outgoing" }));
    assert!(callees.contains("-> Helper  src/engine.cpp:3  (calls at 5)"), "{callees}");

    let types = mcp.tool("type_hierarchy", json!({ "symbol": "Engine" }));
    assert!(types.contains("Engine  src/engine.h:8") && types.contains("Base  src/engine.h:3"), "{types}");
    let derived = mcp.tool("type_hierarchy", json!({ "symbol": "Base", "direction": "subtypes" }));
    assert!(derived.contains("Engine  src/engine.h:8"), "{derived}");

    let refs = mcp.tool("references", json!({ "symbol": "Engine::Run" }));
    assert!(refs.contains("src/main.cpp: 3"), "{refs}");
    let outline = mcp.tool("outline", json!({ "path": "src/engine.h" }));
    assert!(outline.contains("class Engine") && outline.contains("method Run"), "{outline}");

    // ③ lines, and ④ text search, both read right.
    let lines = mcp.tool("read_lines", json!({ "path": "src/engine.cpp", "start": 5, "end": 5 }));
    assert!(
        lines.contains("(cp932, read right)")
            && lines.contains("    5  int Engine::Init() { const char* s = \"表示\";"),
        "{lines}"
    );
    let hits = mcp.tool("search", json!({ "pattern": "表示" }));
    assert!(hits.starts_with("4 matching lines"), "{hits}");
    assert!(hits.contains("src/engine.h:10:   int Init() override; // 表示を初期化する"), "{hits}");

    let health = mcp.tool("diagnose", json!({ "path": "src/engine.cpp" }));
    assert!(health.contains("src/engine.cpp: cp932, in compile_commands.json"), "{health}");
    assert!(health.contains("clangd: 0 errors"), "{health}");

    // A name the index does not know, and one that stands for two symbols, are refusals that say so.
    let unknown = mcp.ask("tools/call", json!({ "name": "definition", "arguments": { "symbol": "Nothing::Here" } }));
    assert_eq!(unknown["result"]["isError"], true);
    let two = mcp.ask("tools/call", json!({ "name": "references", "arguments": { "symbol": "Init" } }));
    let text = two["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("names 2 symbols") && text.contains("src/engine.cpp:4"), "{text}");
}
