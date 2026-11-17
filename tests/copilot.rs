//! Copilot's gate end to end: `mixenc copilot hook` with the events Copilot CLI 1.0 sends (their
//! shapes taken from a real session on a made-up sample), and `mixenc copilot install` into a
//! Copilot folder of the test's own.

mod common;

use std::fs;
use std::path::Path;

use common::Sample;
use serde_json::{Value, json};

fn hook(s: &Sample, event: &str, payload: Value) -> Option<Value> {
    let out = s.mixenc(&s.repo, &["copilot", "hook", event], Some(payload.to_string().as_bytes()));
    assert_eq!(out.status.code(), Some(0), "the hook always exits 0: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    (!text.trim().is_empty()).then(|| serde_json::from_str(&text).unwrap())
}

fn tool(s: &Sample, event: &str, name: &str, args: Value) -> Option<Value> {
    hook(
        s,
        event,
        json!({ "sessionId": "t", "timestamp": 0, "cwd": s.repo.display().to_string(), "toolName": name, "toolArgs": args }),
    )
}

fn denied(answer: Option<Value>) -> String {
    let answer = answer.expect("a decision");
    assert_eq!(answer["permissionDecision"], "deny", "{answer}");
    answer["permissionDecisionReason"].as_str().unwrap().to_string()
}

#[test]
fn reads_go_to_the_mirror_and_built_in_writes_to_the_gate() {
    let s = Sample::new();
    let before: Vec<Vec<u8>> =
        ["engine.h", "engine.cpp", "main.cpp"].iter().map(|f| fs::read(s.repo.join("src").join(f)).unwrap()).collect();
    let engine = s.repo.join(r"src\engine.cpp").display().to_string();

    // A CP932 file is read from its mirror; a UTF-8 one as it is.
    let read = tool(&s, "preToolUse", "view", json!({ "path": engine, "view_range": [5, 5] })).expect("modifiedArgs");
    let mirror = read["modifiedArgs"]["path"].as_str().unwrap();
    assert_eq!(Path::new(mirror), s.mirror("src/engine.cpp"));
    assert_eq!(read["modifiedArgs"]["view_range"], json!([5, 5]));
    assert!(fs::read_to_string(mirror).unwrap().contains("\"表示\""));
    assert_eq!(tool(&s, "preToolUse", "view", json!({ "path": "src/main.cpp" })), None);

    // Built-in writes to a CP932 file stop, whatever the tool.
    let patch = format!(
        "*** Begin Patch\n*** Update File: {engine}\n@@\n int Base::Init() {{ return 0; }}\n+// x\n*** End Patch\n"
    );
    let why = denied(tool(&s, "preToolUse", "apply_patch", json!(patch)));
    assert!(why.contains("is cp932") && why.contains("mixenc MCP server's edit"), "{why}");
    let why =
        denied(tool(&s, "preToolUse", "edit", json!({ "path": "src/engine.h", "old_str": "Run", "new_str": "Go" })));
    assert!(why.contains("engine.h is cp932"), "{why}");
    // The mirror is read-only.
    let into_mirror = s.mirror("src/main.cpp").display().to_string();
    let why = denied(tool(&s, "preToolUse", "create", json!({ "path": into_mirror, "file_text": "x" })));
    assert!(why.contains("is a mirror"), "{why}");
    // Non-ASCII text into a new file, where MSVC would read UTF-8 as CP932.
    let why = denied(tool(&s, "preToolUse", "create", json!({ "path": "src/new.h", "file_text": "// 表\n" })));
    assert!(why.contains("cp932"), "{why}");

    // What a UTF-8 tool writes right goes through.
    assert_eq!(
        tool(&s, "preToolUse", "edit", json!({ "path": "src/main.cpp", "old_str": "e;", "new_str": "e; // 表示" })),
        None
    );
    assert_eq!(tool(&s, "preToolUse", "create", json!({ "path": "src/new.h", "file_text": "#pragma once\n" })), None);
    let elsewhere = std::env::temp_dir().join("mixenc-not-a-workspace.cpp").display().to_string();
    assert_eq!(tool(&s, "preToolUse", "edit", json!({ "path": elsewhere, "new_str": "表" })), None);
    assert_eq!(tool(&s, "preToolUse", "glob", json!({ "pattern": "**/*.cpp" })), None);

    // Input the hook does not understand passes, with exit 0.
    let out = s.mixenc(&s.repo, &["copilot", "hook", "preToolUse"], Some(b"not json"));
    assert_eq!((out.status.code(), out.stdout.is_empty()), (Some(0), true));

    let after: Vec<Vec<u8>> =
        ["engine.h", "engine.cpp", "main.cpp"].iter().map(|f| fs::read(s.repo.join("src").join(f)).unwrap()).collect();
    assert_eq!(before, after, "the hook itself writes no original");
}

#[test]
fn grep_answers_get_the_cp932_lines_read_right() {
    let s = Sample::new();
    let result = |args: Value, text: &str| -> Option<String> {
        let payload = json!({
            "cwd": s.repo.display().to_string(), "toolName": "rg", "toolArgs": args,
            "toolResult": { "resultType": "success", "textResultForLlm": text },
        });
        hook(&s, "postToolUse", payload).map(|a| a["modifiedResult"]["textResultForLlm"].as_str().unwrap().to_string())
    };

    // ripgrep found nothing: the CP932 files hold 表示 as their bytes, not as UTF-8.
    let found =
        result(json!({ "pattern": "表示", "paths": "src", "output_mode": "content", "-n": true }), "No matches found.")
            .expect("completed");
    assert!(
        found.contains("src\\engine.cpp:5:int Engine::Init() { const char* s = \"表示\"; return Helper(); }"),
        "{found}"
    );
    assert!(found.contains("src\\engine.h:10:  int Init() override; // 表示を初期化する"), "{found}");
    assert!(!found.contains("No matches found."), "{found}");

    // What ripgrep read wrong in a CP932 file is replaced; the UTF-8 file's lines stay.
    let garbled = "src\\engine.cpp:5:int Engine::Init() { const char* s = \"\u{FFFD}\\\u{FFFD}\u{FFFD}\"; return Helper(); }\r\nsrc\\main.cpp:3:int main() { Engine e; return e.Run(); }";
    let fixed = result(json!({ "pattern": "Init|Run", "paths": "src", "output_mode": "content", "-n": true }), garbled)
        .unwrap();
    assert!(!fixed.contains('\u{FFFD}'), "{fixed}");
    assert!(fixed.contains("src\\main.cpp:3:int main()"), "{fixed}");
    assert!(fixed.contains("src\\engine.cpp:6:int Engine::Run()"), "{fixed}");

    let counted =
        result(json!({ "pattern": "表示", "paths": "src", "output_mode": "count" }), "No matches found.").unwrap();
    assert!(counted.starts_with("src\\engine.cpp:2\nsrc\\engine.h:2"), "{counted}");
    let files = result(json!({ "pattern": "表示", "paths": "", "glob": "*.h" }), "No matches found.").unwrap();
    assert!(files.starts_with(".\\src\\engine.h\n("), "{files}");

    // Nothing to add where no file is CP932.
    assert_eq!(result(json!({ "pattern": "x", "paths": "src/main.cpp" }), "src/main.cpp"), None);
}

#[test]
fn a_session_starts_with_the_way_to_read_and_write() {
    let s = Sample::new();
    let context = hook(&s, "sessionStart", json!({ "cwd": s.repo.display().to_string(), "source": "new" })).unwrap();
    let text = context["additionalContext"].as_str().unwrap();
    assert!(text.contains("mixenc workspace") && text.contains("repositories: r1"), "{text}");
    assert!(text.contains("edit and create") && text.contains("mixenc-analyst"), "{text}");
    assert!(text.contains("mixenc doctor:"), "{text}");
    let outside = hook(&s, "sessionStart", json!({ "cwd": std::env::temp_dir().display().to_string() }));
    assert_eq!(outside, None);
}

#[test]
fn install_keeps_other_settings_and_uninstall_puts_them_back() {
    let s = Sample::new();
    let home = &s.copilot_home;
    let lsp_before = json!({
        "lspServers": {
            "clangd": { "command": "clangd", "args": ["--background-index"], "fileExtensions": { ".cpp": "cpp" } },
            "rust-analyzer": { "command": "rust-analyzer", "fileExtensions": { ".rs": "rust" } },
        }
    });
    common::write(&home.join("lsp-config.json"), lsp_before.to_string().as_bytes());
    common::write(&home.join("mcp-config.json"), br#"{"mcpServers":{"other":{"command":"x"}}}"#);
    let home_arg = home.display().to_string();
    let run = |args: &[&str]| s.mixenc(&s.ws, &[args, &["--home", &home_arg]].concat(), None);
    let read = |name: &str| -> Value { serde_json::from_slice(&fs::read(home.join(name)).unwrap()).unwrap() };

    assert_eq!(run(&["copilot", "status"]).status.code(), Some(1));
    for _ in 0..2 {
        let out = run(&["copilot", "install"]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let exe = mixenc::paths::display(&mixenc::paths::canonical(Path::new(env!("CARGO_BIN_EXE_mixenc"))));
    let hooks = read("hooks/mixenc.json");
    assert_eq!(hooks["hooks"]["preToolUse"][0]["exec"], exe.as_str());
    assert_eq!(hooks["hooks"]["postToolUse"][0]["args"], json!(["copilot", "hook", "postToolUse"]));
    let agent = fs::read_to_string(home.join("agents/mixenc-analyst.agent.md")).unwrap();
    assert!(agent.contains("mixenc/call_hierarchy") && !agent.contains("mixenc/edit"));
    let mcp = read("mcp-config.json");
    assert_eq!(
        (mcp["mcpServers"]["other"]["command"].as_str(), mcp["mcpServers"]["mixenc"]["args"].clone()),
        (Some("x"), json!(["mcp"]))
    );
    let lsp = read("lsp-config.json");
    assert_eq!(lsp["lspServers"]["clangd"]["command"], exe.as_str());
    assert_eq!(
        lsp["lspServers"]["clangd"]["args"],
        json!(["clangd", "--background-index"]),
        "installed twice, clangd once"
    );
    assert_eq!(lsp["lspServers"]["clangd"]["fileExtensions"], json!({ ".cpp": "cpp" }));
    assert_eq!(lsp["lspServers"]["rust-analyzer"], lsp_before["lspServers"]["rust-analyzer"]);
    assert_eq!(run(&["copilot", "status"]).status.code(), Some(0));

    // The doctor sees it (through COPILOT_HOME).
    let doctor = String::from_utf8(s.mixenc(&s.ws, &["doctor"], None).stdout).unwrap();
    assert!(doctor.contains("Copilot's hooks, the analysis agent and the MCP server are installed"), "{doctor}");

    assert!(run(&["copilot", "uninstall"]).status.success());
    assert_eq!(read("lsp-config.json"), lsp_before);
    assert_eq!(read("mcp-config.json"), json!({ "mcpServers": { "other": { "command": "x" } } }));
    assert!(!home.join("hooks/mixenc.json").exists() && !home.join("agents/mixenc-analyst.agent.md").exists());
    let doctor = String::from_utf8(s.mixenc(&s.ws, &["doctor"], None).stdout).unwrap();
    assert!(doctor.contains("mixenc copilot install"), "{doctor}");
}

#[test]
fn the_intro_names_the_agent() {
    let s = Sample::new();
    let en = String::from_utf8(s.mixenc(&s.ws, &["copilot", "intro"], None).stdout).unwrap();
    let ja = String::from_utf8(s.mixenc(&s.ws, &["copilot", "intro", "--ja"], None).stdout).unwrap();
    assert!(en.contains("mixenc-analyst") && ja.contains("mixenc-analyst") && ja.contains("結論"));
}
