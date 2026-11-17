//! The front gate and the commit gate, end to end with git and the mixenc program: an edit through
//! the gate keeps every other line's bytes and passes the pre-commit; an editor's save that changes
//! an untouched line or the encoding is stopped; a pre-commit that was there runs first and comes
//! back on uninstall; `core.hooksPath` keeps mixenc out. Then the MCP server's tools. Git reads no
//! settings of this PC (`GIT_CONFIG_GLOBAL`, `GIT_CONFIG_NOSYSTEM`), and every sample is made here.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

fn sjis(s: &str) -> Vec<u8> {
    let (bytes, _, unmappable) = encoding_rs::SHIFT_JIS.encode(s);
    assert!(!unmappable, "{s}");
    bytes.into_owned()
}

struct Ws {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    gitconfig: PathBuf,
}

impl Ws {
    fn run(&self, program: &str, dir: &Path, args: &[&str]) -> Output {
        Command::new(program)
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }

    fn mixenc(&self, args: &[&str]) -> Output {
        self.run(env!("CARGO_BIN_EXE_mixenc"), &self.root, args)
    }

    fn git(&self, args: &[&str]) -> Output {
        self.run("git", &self.repo, args)
    }

    fn hooks(&self) -> PathBuf {
        self.repo.join(".git").join("hooks")
    }
}

fn ok(out: Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stopped(out: Output) -> String {
    assert!(!out.status.success(), "went through: {}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `a.cpp` is CP932 with `Ⅰ` in its IBM form (`FA 4A`) on a line no one edits.
fn a_cpp(a: &str, b: &str) -> Vec<u8> {
    [&sjis(&format!("// 表示の設定\r\n{a}\r\n"))[..], b"// \xFA\x4A\r\n", &sjis(&format!("{b}\r\n"))].concat()
}

fn workspace() -> Ws {
    let tmp = tempfile::tempdir().unwrap();
    let base = mixenc::paths::canonical(tmp.path());
    let root = base.join("ws");
    let repo = root.join("repos").join("cloneA");
    fs::create_dir_all(&repo).unwrap();
    let gitconfig = base.join("gitconfig");
    fs::write(
        &gitconfig,
        "[user]\n\tname = mixenc test\n\temail = test@example.invalid\n[commit]\n\tgpgsign = false\n",
    )
    .unwrap();
    fs::write(repo.join("a.cpp"), a_cpp("int a = 1;", "int b = 2; // 能")).unwrap();
    fs::write(repo.join("plain.h"), b"#pragma once\r\nint c;\r\n").unwrap();
    let ws = Ws { _tmp: tmp, root, repo, gitconfig };
    ok(ws.git(&["init", "-q"]));
    ok(ws.git(&["add", "."]));
    ok(ws.git(&["commit", "-q", "-m", "first"]));
    ok(ws.mixenc(&["init", "--repo", "repos/cloneA"]));
    ok(ws.mixenc(&["sync"]));
    ws
}

#[test]
fn the_front_gate_keeps_other_lines_and_the_commit_gate_stops_what_does_not() {
    let ws = workspace();
    // A pre-commit that was there: it leaves a mark and lets the commit through.
    fs::write(ws.hooks().join("pre-commit"), "#!/bin/sh\necho ran >> .git/before-ran\nexit 0\n").unwrap();
    let out = ok(ws.mixenc(&["hooks", "install"]));
    assert!(out.contains("cloneA: installed (the pre-commit that was there before runs first)"), "{out}");
    let doctor = String::from_utf8_lossy(&ws.mixenc(&["doctor"]).stdout).into_owned();
    assert!(doctor.contains("the commit gate (pre-commit) is installed"), "{doctor}");

    // The front gate, given the mirror's path: one line written, the IBM form kept.
    let mirror = ws.root.join("mirror").join("cloneA").join("a.cpp");
    let out = ok(ws.mixenc(&["edit", &mirror.to_string_lossy(), "--old", "int a = 1;", "--new", "int a = 1; // 変更"]));
    assert!(out.contains("(cp932): line 2; every other line kept its bytes"), "{out}");
    assert_eq!(fs::read(ws.repo.join("a.cpp")).unwrap(), a_cpp("int a = 1; // 変更", "int b = 2; // 能"));
    assert_eq!(
        fs::read_to_string(&mirror).unwrap(),
        "// 表示の設定\r\nint a = 1; // 変更\r\n// Ⅰ\r\nint b = 2; // 能\r\n"
    );
    ok(ws.git(&["commit", "-q", "-a", "-m", "through the gate"]));
    assert_eq!(fs::read_to_string(ws.repo.join(".git").join("before-ran")).unwrap(), "ran\n");

    // An editor that writes the whole file through Unicode: Ⅰ comes back in NEC's form (87 54).
    assert_eq!(sjis("Ⅰ"), b"\x87\x54");
    let saved = sjis("// 表示の設定\r\nint a = 1; // 変更\r\n// Ⅰ\r\nint b = 3; // 能\r\n");
    fs::write(ws.repo.join("a.cpp"), &saved).unwrap();
    let err = stopped(ws.git(&["commit", "-q", "-a", "-m", "editor"]));
    assert!(err.contains("a.cpp (cp932)"), "{err}");
    assert!(err.contains("line 3: the text is the same but its bytes changed: Ⅰ FA 4A -> 87 54"), "{err}");
    // Saved as UTF-8.
    fs::write(ws.repo.join("a.cpp"), "// 表示の設定\r\nint a = 1; // 変更\r\n// Ⅰ\r\nint b = 3; // 能\r\n").unwrap();
    let err = stopped(ws.git(&["commit", "-q", "-a", "-m", "utf8"]));
    assert!(err.contains("the encoding changed: cp932 -> utf8"), "{err}");
    assert!(err.contains("line 1: the text is the same but its bytes changed: 表 95 5C -> E8 A1 A8"), "{err}");
    // Japanese added to an ASCII header as UTF-8 (VS Code's default), where MSVC reads CP932.
    ok(ws.git(&["checkout", "-q", "HEAD", "--", "a.cpp"]));
    fs::write(ws.repo.join("plain.h"), "#pragma once\r\nint c; // 表\r\n").unwrap();
    let err = stopped(ws.git(&["commit", "-q", "-a", "-m", "utf8 in ascii"]));
    assert!(err.contains("plain.h (ascii -> utf8)"), "{err}");
    assert!(err.contains("write it in cp932"), "{err}");
    fs::write(ws.repo.join("plain.h"), sjis("#pragma once\r\nint c; // 表\r\n")).unwrap();
    ok(ws.git(&["commit", "-q", "-a", "-m", "cp932 in ascii"]));

    // The pre-commit that was there stops a commit by itself, before mixenc looks.
    fs::write(ws.hooks().join(mixenc::hooks::BEFORE), "#!/bin/sh\necho no from the old hook >&2\nexit 3\n").unwrap();
    fs::write(ws.repo.join("plain.h"), sjis("#pragma once\r\nint c; // 表です\r\n")).unwrap();
    let err = stopped(ws.git(&["commit", "-q", "-a", "-m", "old hook says no"]));
    assert!(err.contains("no from the old hook") && !err.contains("mixenc"), "{err}");

    // Uninstalling puts it back as it was.
    let out = ok(ws.mixenc(&["hooks", "uninstall"]));
    assert!(out.contains("cloneA: not installed (another pre-commit is there)"), "{out}");
    assert_eq!(
        fs::read_to_string(ws.hooks().join("pre-commit")).unwrap(),
        "#!/bin/sh\necho no from the old hook >&2\nexit 3\n"
    );
    assert!(!ws.hooks().join(mixenc::hooks::BEFORE).exists());
}

#[test]
fn core_hooks_path_keeps_mixenc_out() {
    let ws = workspace();
    ok(ws.git(&["config", "core.hooksPath", "managed-hooks"]));
    let out = ws.mixenc(&["hooks", "install"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("not installed: core.hooksPath is set (managed-hooks)"), "{text}");
    assert!(!ws.hooks().join("pre-commit").exists());
    assert!(!ws.repo.join("managed-hooks").exists());
    let doctor = String::from_utf8_lossy(&ws.mixenc(&["doctor"]).stdout).into_owned();
    assert!(doctor.contains("core.hooksPath is set (managed-hooks)"), "{doctor}");
}

#[test]
fn the_mcp_tools_write_in_each_file_s_encoding() {
    let ws = workspace();
    ok(ws.mixenc(&["hooks", "install"]));
    let mut child = Command::new(env!("CARGO_BIN_EXE_mixenc"))
        .arg("mcp")
        .current_dir(&ws.root)
        .env("GIT_CONFIG_GLOBAL", &ws.gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut send = |message: Value| -> Option<Value> {
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
        message.get("id")?;
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        Some(serde_json::from_str(&line).unwrap())
    };
    let init = send(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } } }))
    .unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "mixenc");
    assert!(send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
    let mut call = |id: u32, name: &str, arguments: Value| -> (bool, String) {
        let reply =
            send(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } }))
                .unwrap();
        let result = &reply["result"];
        (result["isError"].as_bool().unwrap(), result["content"][0]["text"].as_str().unwrap().to_string())
    };
    let a = ws.repo.join("a.cpp").to_string_lossy().into_owned();

    let (error, text) =
        call(2, "edit", json!({ "path": a, "old_string": "int b = 2; // 能", "new_string": "int b = 2; // 能力" }));
    assert!(!error && text.contains("(cp932): line 4"), "{text}");
    assert_eq!(fs::read(&a).unwrap(), a_cpp("int a = 1;", "int b = 2; // 能力"));

    let (error, text) = call(3, "edit", json!({ "path": a, "old_string": "// 能力", "new_string": "// 😀" }));
    assert!(error && text.contains("😀 U+1F600 on line 4"), "{text}");
    assert_eq!(fs::read(&a).unwrap(), a_cpp("int a = 1;", "int b = 2; // 能力"), "nothing written");

    // A new header follows the files around it (a.cpp: CP932).
    let new_h = ws.repo.join("sub").join("new.h");
    let (error, text) =
        call(4, "create", json!({ "path": new_h.to_string_lossy(), "content": "#pragma once\n// 新しい\n" }));
    assert!(!error && text.contains("(cp932)"), "{text}");
    assert_eq!(fs::read(&new_h).unwrap(), sjis("#pragma once\n// 新しい\n"));
    let mirror = ws.root.join("mirror").join("cloneA").join("sub").join("new.h");
    assert_eq!(fs::read_to_string(mirror).unwrap(), "#pragma once\n// 新しい\n");

    drop(call);
    drop(send);
    drop(input);
    child.wait().unwrap();
    ok(ws.git(&["add", "-A"]));
    ok(ws.git(&["commit", "-q", "-m", "through the MCP tools"]));
}
