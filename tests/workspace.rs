//! Stage 5's and 6's commands end to end on the sample: the `.code-workspace`, the file list the
//! extension reads, the override table, reading a file another way, the deep doctor, a full rebuild,
//! and settings files kept in a vault in git with the mirrors outside it.

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
    let v: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let exe = mixenc::paths::forward(
        &mixenc::paths::canonical(std::path::Path::new(env!("CARGO_BIN_EXE_mixenc"))).to_string_lossy(),
    );
    assert_eq!(v["folders"], json!([{ "name": "r1", "path": "repos/r1" }]));
    assert_eq!(v["settings"]["mixenc.configs"], json!(["mixenc.json"]));
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

#[test]
fn a_workspace_kept_elsewhere_is_found_from_its_repository() {
    let s = Sample::new();
    let base = s.ws.parent().unwrap().to_path_buf();
    // A repository with no mixenc.json above it, and its workspace in a folder of its own.
    let repo = base.join("plain").join("r2");
    common::write(&repo.join("a.cpp"), &common::sjis("// 表\r\nint a;\r\n"));
    let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&repo).status().unwrap();
    assert!(git(&["init", "-q"]).success() && git(&["add", "-A"]).success());
    let home = base.join("home-ws");
    fs::create_dir_all(&home).unwrap();
    ok(s.mixenc(&home, &["init", "--repo", &repo.display().to_string()], None));
    let register = fs::read_to_string(base.join("mixenc-home").join("workspaces.json")).unwrap();
    assert!(register.contains("home-ws"), "{register}");

    // From inside the repository, mixenc finds that workspace.
    ok(s.mixenc(&repo, &["sync"], None));
    assert_eq!(fs::read_to_string(home.join("mirror").join("r2").join("a.cpp")).unwrap(), "// 表\r\nint a;\r\n");
    let files = json_of(s.mixenc(&repo, &["files", "--json"], None));
    assert_eq!(files["files"][0]["judged"], "cp932");
}

/// Stage 6: settings files in a vault kept in git, one per repository; the mirrors outside it; the
/// person's .code-workspace gets mixenc's items.
#[test]
fn settings_in_a_vault_and_mirrors_outside_it() {
    let s = Sample::new();
    let base = s.ws.parent().unwrap().to_path_buf();
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    let mut repos = Vec::new();
    for name in ["r2", "r3"] {
        let repo = base.join("plain").join(name);
        common::write(&repo.join("a.cpp"), &common::sjis(&format!("// 表 {name}\r\nint a;\r\n")));
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "-A"]);
        repos.push(repo);
    }
    let vault = base.join("vault");
    fs::create_dir_all(&vault).unwrap();
    git(&vault, &["init", "-q"]);

    // Without --mirror the mirrors would be in the vault's git: refused.
    let refused = s.mixenc(&base, &["init", "--config", "vault/r2.mixenc.json", "--repo", "plain/r2"], None);
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("git work tree"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!vault.join("r2.mixenc.json").exists());
    for name in ["r2", "r3"] {
        let file = format!("vault/{name}.mixenc.json");
        let repo = format!("plain/{name}");
        ok(s.mixenc(&base, &["init", "--config", &file, "--repo", &repo, "--mirror", "mirror2"], None));
    }
    let config: Value = serde_json::from_slice(&fs::read(vault.join("r2.mixenc.json")).unwrap()).unwrap();
    assert!(config["mirror"].as_str().unwrap().ends_with("/mirror2"), "{config}");

    // From inside a repository, mixenc finds its settings file through the register.
    ok(s.mixenc(&repos[0], &["sync"], None));
    assert_eq!(fs::read_to_string(base.join("mirror2/r2/a.cpp")).unwrap(), "// 表 r2\r\nint a;\r\n");
    let place = json_of(s.mixenc(&repos[1], &["where", "--json"], None));
    assert!(place["config"].as_str().unwrap().ends_with("r3.mixenc.json"), "{place}");
    // Copilot's hooks and the commit gate start from the repository, and find it too.
    let start = s.mixenc(
        &repos[1],
        &["copilot", "hook", "sessionStart"],
        Some(json!({ "cwd": repos[1].display().to_string(), "source": "new" }).to_string().as_bytes()),
    );
    let context: Value = serde_json::from_slice(&start.stdout).unwrap();
    assert!(context["additionalContext"].as_str().unwrap().contains("r3.mixenc.json"), "{context}");
    let a = repos[1].join("a.cpp");
    fs::write(&a, "// 表 r3\r\nint a;\r\n").unwrap();
    git(&repos[1], &["add", "a.cpp"]);
    let gate = s.mixenc(&repos[1], &["hooks", "pre-commit"], None);
    assert_eq!(
        gate.status.code(),
        Some(1),
        "the gate stops CP932 turned UTF-8: {}",
        String::from_utf8_lossy(&gate.stderr)
    );
    fs::write(&a, common::sjis("// 表 r3\r\nint a;\r\n")).unwrap();
    git(&repos[1], &["add", "a.cpp"]);

    // In the vault, with two settings files and no repository there, it asks which.
    let which = s.mixenc(&vault, &["doctor"], None);
    assert_eq!(which.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&which.stderr).contains("--config"), "{}", String::from_utf8_lossy(&which.stderr));

    // The person's .code-workspace: a dry run first, then the items, then nothing more to add.
    let ws_file = vault.join("dev.code-workspace");
    fs::write(&ws_file, "{\n  // mine\n  \"folders\": [{ \"path\": \".\" }],\n}\n").unwrap();
    let add = |extra: &[&str]| {
        let mut args = vec![
            "workspace",
            "dev.code-workspace",
            "--config",
            "r2.mixenc.json",
            "--config",
            "r3.mixenc.json",
            "--json",
        ];
        args.extend_from_slice(extra);
        json_of(s.mixenc(&vault, &args, None))
    };
    let dry = add(&["--dry-run"]);
    assert_eq!(dry["written"], false);
    assert!(fs::read_to_string(&ws_file).unwrap().ends_with("[{ \"path\": \".\" }],\n}\n"));
    assert_eq!(add(&[])["written"], true);
    assert_eq!(add(&[])["changes"], json!([]));
    let text = fs::read_to_string(&ws_file).unwrap();
    assert!(text.contains("// mine") && text.contains("r3.mixenc.json"), "{text}");
    assert!(add(&[])["copilot"].as_str().unwrap().contains("--add-dir"));

    // git in the vault sees the settings and the .code-workspace, never a mirror.
    ok(s.mixenc(&repos[1], &["sync"], None));
    let status = git(&vault, &["status", "--porcelain", "-uall"]);
    let mut seen: Vec<&str> = status.lines().map(|l| l.rsplit('/').next().unwrap().trim_start_matches("?? ")).collect();
    seen.sort_unstable();
    assert_eq!(seen, ["dev.code-workspace", "r2.mixenc.json", "r3.mixenc.json"], "{status}");

    // A settings file that gives r3 the mirror of r2 does not turn one into the other.
    fs::write(
        vault.join("bad.mixenc.json"),
        r#"{"mirror": "../mirror2", "repos": [{"path": "../plain/r3", "mirror": "../mirror2/r2"}]}"#,
    )
    .unwrap();
    let taken = s.mixenc(&vault, &["sync", "--config", "bad.mixenc.json"], None);
    assert_eq!(taken.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&taken.stderr).contains("is the mirror of"),
        "{}",
        String::from_utf8_lossy(&taken.stderr)
    );
    let rebuild = s.mixenc(&vault, &["sync", "--rebuild", "--config", "bad.mixenc.json"], None);
    assert_eq!(rebuild.status.code(), Some(2), "a rebuild does not take it either");
    assert_eq!(fs::read_to_string(base.join("mirror2/r2/a.cpp")).unwrap(), "// 表 r2\r\nint a;\r\n");
}
