//! The sample the stage-4 tests share: a repository with a base class and a derived one in CP932
//! (CRLF), a UTF-8 main.cpp that calls into them, and a compile_commands.json, mirrored.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

pub const ENGINE_H: &str = "#pragma once\n// エンジンの表示\nclass Base {\npublic:\n  virtual ~Base() {}\n  virtual int Init();\n};\nclass Engine : public Base {\npublic:\n  int Init() override; // 表示を初期化する\n  int Run();\n};\n";
/// Line 5 holds `"表示"`, whose CP932 bytes have the 0x5C trap (95 5C).
pub const ENGINE_CPP: &str = "#include \"engine.h\"\n// 能力の表示\nstatic int Helper() { return 1; }\nint Base::Init() { return 0; }\nint Engine::Init() { const char* s = \"表示\"; return Helper(); }\nint Engine::Run() { return Init() + 1; }\n";
pub const MAIN_CPP: &str = "#include \"engine.h\"\n// UTF-8 のファイル\nint main() { Engine e; return e.Run(); }\n";

pub fn sjis(s: &str) -> Vec<u8> {
    encoding_rs::SHIFT_JIS.encode(s).0.into_owned()
}

pub fn crlf(s: &str) -> String {
    s.replace('\n', "\r\n")
}

pub fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

pub struct Sample {
    _tmp: tempfile::TempDir,
    pub ws: PathBuf,
    pub repo: PathBuf,
    /// A Copilot folder of its own, so that this PC's is never read or written.
    pub copilot_home: PathBuf,
}

impl Sample {
    pub fn new() -> Sample {
        let tmp = tempfile::tempdir().unwrap();
        let base = mixenc::paths::canonical(tmp.path());
        let ws = base.join("ws");
        let repo = ws.join("repos").join("r1");
        let src = repo.join("src");
        write(&src.join("engine.h"), &sjis(&crlf(ENGINE_H)));
        write(&src.join("engine.cpp"), &sjis(&crlf(ENGINE_CPP)));
        write(&src.join("main.cpp"), crlf(MAIN_CPP).as_bytes());
        let db: Vec<Value> = ["engine.cpp", "main.cpp"]
            .iter()
            .map(|f| {
                json!({
                    "directory": src.display().to_string(),
                    "file": src.join(f).display().to_string(),
                    "arguments": ["cl.exe", "/c", format!("/I{}", src.display()), "/source-charset:shift_jis", f],
                })
            })
            .collect();
        write(&repo.join("compile_commands.json"), serde_json::to_string(&db).unwrap().as_bytes());
        write(&repo.join(".gitignore"), b"compile_commands.json\n");
        for args in [&["init", "-q"][..], &["add", "-A"]] {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", base.join("gitconfig"))
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }
        write(&ws.join("mixenc.json"), br#"{"repos": [{"path": "repos/r1", "db": "repos/r1/compile_commands.json"}]}"#);
        let copilot_home = base.join("copilot-home");
        fs::create_dir_all(&copilot_home).unwrap();
        let sample = Sample { _tmp: tmp, ws, repo, copilot_home };
        let out = sample.mixenc(&sample.ws, &["sync"], None);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        sample
    }

    /// Runs mixenc in `dir`, with `input` on stdin.
    pub fn mixenc(&self, dir: &Path, args: &[&str], input: Option<&[u8]>) -> Output {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_mixenc"))
            .args(args)
            .current_dir(dir)
            .env("COPILOT_HOME", &self.copilot_home)
            .env("MIXENC_HOME", self.copilot_home.with_file_name("mixenc-home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.unwrap_or_default()).unwrap();
        drop(stdin);
        child.wait_with_output().unwrap()
    }

    pub fn mirror(&self, rel: &str) -> PathBuf {
        mixenc::paths::join_rel(&self.ws.join("mirror").join("r1"), rel)
    }
}

/// clangd for the end-to-end tests: `MIXENC_TEST_CLANGD`, LLVM's, Neovim's mason's, or one on PATH.
pub fn find_clangd() -> Option<PathBuf> {
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
        eprintln!("no clangd: the end-to-end tests that need it are skipped");
    }
    found
}
