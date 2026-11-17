//! Stage 5's commands end to end on the sample: the `.code-workspace`, the file list the extension
//! reads, the override table, reading a file another way, the deep doctor and a full rebuild.

mod common;

use std::fs;

use common::Sample;
use serde_json::{Value, json};

fn ok(out: std::process::Output) -> String {
    assert!(out.status.code().is_some_and(|c| c <= 1), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn json_of(out: std::process::Output) -> Value {
    serde_json::from_str(&ok(out)).unwrap()
}

#[test]
fn the_code_workspace_points_clangd_at_the_entry() {
    let s = Sample::new();
    let written = json_of(s.mixenc(&s.ws, &["workspace", "--json"], None));
    let path = written["path"].as_str().unwrap();
    assert!(path.ends_with("r1.code-workspace"), "{path}");
    let text = fs::read_to_string(path).unwrap();
    assert!(text.starts_with(mixenc::codews::MARKER));
    let v: Value = serde_json::from_str(text.split_once('\n').unwrap().1).unwrap();
    let exe = mixenc::paths::forward(
        &mixenc::paths::canonical(std::path::Path::new(env!("CARGO_BIN_EXE_mixenc"))).to_string_lossy(),
    );
    assert_eq!(v["folders"], json!([{ "name": "r1", "path": "repos/r1" }]));
    assert_eq!(v["settings"]["clangd.path"].as_str().map(str::to_lowercase), Some(exe.to_lowercase()));
    assert_eq!(v["settings"]["C_Cpp.intelliSenseEngine"], "disabled");
    assert_eq!(v["extensions"]["recommendations"], json!(mixenc::codews::RECOMMENDED));
}

#[test]
fn files_overrides_and_readings() {
    let s = Sample::new();
    // EUC-JP that reads as CP932 too: it waits for a person.
    common::write(&s.repo.join("docs/x.txt"), &encoding_rs::EUC_JP.encode("アイウ\n").0);
    let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&s.repo).status().unwrap();
    assert!(git(&["add", "-A"]).success());
    ok(s.mixenc(&s.ws, &["sync"], None));

    let files = json_of(s.mixenc(&s.ws, &["files", "--json"], None));
    let by_name = |files: &Value, name: &str| -> Value {
        files["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"].as_str().unwrap().ends_with(name))
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(by_name(&files, "engine.cpp")["judged"], "cp932");
    assert_eq!(by_name(&files, "main.cpp"), Value::Null, "UTF-8 files are not listed");
    let x = by_name(&files, "x.txt");
    assert_eq!(
        (x["judged"].as_str(), x["mirrored"].as_bool(), x["problem"]["kind"].as_str()),
        (Some("ambiguous-cp932-euc-jp"), Some(false), Some("undecided"))
    );

    // The two readings, side by side in the extension.
    let euc = json_of(s.mixenc(&s.ws, &["read", "repos/r1/docs/x.txt", "--as", "euc-jp", "--json"], None));
    assert_eq!(euc["lines"], json!(["アイウ"]));
    let cp932 = json_of(s.mixenc(&s.ws, &["read", "repos/r1/docs/x.txt", "--as", "cp932", "--json"], None));
    assert_ne!(cp932["lines"], euc["lines"]);

    ok(s.mixenc(&s.ws, &["override", "repos/r1/docs/x.txt", "euc-jp"], None));
    let config: Value = serde_json::from_slice(&fs::read(s.ws.join("mixenc.json")).unwrap()).unwrap();
    assert_eq!(
        config["overrides"]["repos/r1/docs/x.txt"],
        json!({ "as": "euc-jp", "judged": "ambiguous-cp932-euc-jp" })
    );
    assert_eq!(config["repos"][0]["db"], "repos/r1/compile_commands.json", "the rest of mixenc.json is kept");
    let x = by_name(&json_of(s.mixenc(&s.ws, &["files", "--json"], None)), "x.txt");
    assert_eq!((x["read_as"].as_str(), x["mirrored"].as_bool()), (Some("euc-jp"), Some(true)), "mirrored at once");
    assert_eq!(fs::read_to_string(s.mirror("docs/x.txt")).unwrap(), "アイウ\n");
    // Read without --as follows the override.
    assert_eq!(json_of(s.mixenc(&s.ws, &["read", "repos/r1/docs/x.txt", "--json"], None))["lines"], json!(["アイウ"]));

    ok(s.mixenc(&s.ws, &["override", "repos/r1/docs/x.txt", "--remove"], None));
    let config: Value = serde_json::from_slice(&fs::read(s.ws.join("mixenc.json")).unwrap()).unwrap();
    assert!(config.get("overrides").is_none_or(|o| o.as_object().unwrap().is_empty()));
    assert!(s.mixenc(&s.ws, &["override", "repos/r1/docs/x.txt", "binary"], None).status.code() == Some(2));
}

#[test]
fn a_rebuild_writes_every_mirror_again() {
    let s = Sample::new();
    let sync = json_of(s.mixenc(&s.ws, &["sync", "--rebuild", "--json"], None));
    let repo = &sync["repos"][0]["roots"][0];
    assert_eq!(repo["files"], repo["written"], "{sync}");
}

#[test]
fn the_deep_doctor_holds_clangd_errors_against_its_baseline() {
    let Some(clangd) = common::find_clangd() else { return };
    let s = Sample::new();
    let deep = |extra: &[&str]| -> Value {
        let mut args = vec!["doctor", "--deep", "--json"];
        args.extend_from_slice(extra);
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_mixenc"));
        cmd.args(&args).current_dir(&s.ws).env("MIXENC_CLANGD", &clangd).env("COPILOT_HOME", &s.copilot_home);
        let out = cmd.output().unwrap();
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("progress 2/2"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let messages = |report: &Value, severity: &str| -> Vec<String> {
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["severity"] == severity)
            .map(|i| i["message"].as_str().unwrap().to_string())
            .collect()
    };
    let first = deep(&[]);
    assert_eq!(first["summary"]["units"], 2);
    assert!(
        messages(&first, "ok").iter().any(|m| m.starts_with("baseline taken: 2 translation units, 0 with errors")),
        "{first}"
    );
    assert!(messages(&first, "ok").iter().any(|m| m == "every header is included by a translation unit"), "{first}");

    // A CP932 line that breaks the code, and a header nobody includes.
    let engine = s.repo.join("src/engine.cpp");
    let mut bytes = fs::read(&engine).unwrap();
    bytes.extend(common::sjis("int broken() { return 表; }\r\n"));
    fs::write(&engine, bytes).unwrap();
    common::write(&s.repo.join("src/lonely.h"), b"#pragma once\r\n");
    assert!(std::process::Command::new("git").args(["add", "-A"]).current_dir(&s.repo).status().unwrap().success());
    ok(s.mixenc(&s.ws, &["sync"], None));
    let second = deep(&[]);
    let broken = messages(&second, "broken");
    assert_eq!(broken, ["1 translation units have more errors in clangd than at the baseline"], "{second}");
    assert!(
        messages(&second, "notice").iter().any(|m| m.starts_with("1 headers are included by no translation unit")),
        "{second}"
    );

    let taken = deep(&["--update-baseline"]);
    assert_eq!(messages(&taken, "broken").len(), 1, "this run still reports against the old baseline");
    let third = deep(&[]);
    assert!(messages(&third, "broken").is_empty(), "{third}");
}
