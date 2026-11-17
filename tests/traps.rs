//! The trap samples, end to end: a workspace with a git repository whose files fall into every trap
//! mixenc knows, an include folder outside it, and a database; then sync, doctor, overrides and
//! changes. Every sample is made here; nothing comes from a real repository.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use mixenc::config::Workspace;
use mixenc::doctor::{self, Severity};
use mixenc::sync;

fn sjis(s: &str) -> Vec<u8> {
    let (bytes, _, unmappable) = encoding_rs::SHIFT_JIS.encode(s);
    assert!(!unmappable, "{s}");
    bytes.into_owned()
}

fn utf16(s: &str, little_endian: bool, bom: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if bom {
        out.extend(if little_endian { [0xFF, 0xFE] } else { [0xFE, 0xFF] });
    }
    for u in s.encode_utf16() {
        out.extend(if little_endian { u.to_le_bytes() } else { u.to_be_bytes() });
    }
    out
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git").args(args).current_dir(repo).status().unwrap();
    assert!(status.success(), "git {args:?}");
}

struct Fixture {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    repo: PathBuf,
    ext: PathBuf,
    deep: String,
    long_name: String,
}

impl Fixture {
    fn mirror(&self, rel: &str) -> PathBuf {
        mixenc::paths::join_rel(&self.ws.join("mirror").join("cloneA"), rel)
    }

    fn read_mirror(&self, rel: &str) -> String {
        String::from_utf8(fs::read(self.mirror(rel)).unwrap()).unwrap()
    }

    fn workspace(&self) -> Workspace {
        Workspace::find(&self.ws).unwrap()
    }
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let repo = ws.join("repos").join("cloneA");
    let ext = tmp.path().join("vendor").join("include");
    fs::create_dir_all(&repo).unwrap();

    // The 0x5C trap: comments ending in 表 (95 5C), 能 (94 5C) and ソ (83 5C), each before a declaration.
    write(
        &repo.join("src/traps.cpp"),
        &sjis(
            "#include \"hyo.h\"\r\n// 表\r\nint after_hyo();\r\n// 能\r\nint after_nou();\r\n// ソ\r\nint after_so();\r\nint main() { return hidden(1); }\r\n",
        ),
    );
    write(&repo.join("src/hyo.h"), &sjis("#pragma once\r\n// 表\r\nint hidden(int);\r\n"));
    // 表示 in a string literal: 95 5C 8E A6.
    write(&repo.join("src/literal.cpp"), &sjis("const char* s = \"表示\";\n"));
    // A lead byte with no mapping before 0x5C, and a lead byte cut at the end of the line.
    write(&repo.join("src/undefined.h"), b"// \x85\x5C\r\nint after_undefined();\r\n");
    write(&repo.join("src/cut.h"), b"// \x82\r\nint after_cut();\r\n");
    // BOMs and UTF-16.
    write(&repo.join("src/bom.h"), &[&b"\xEF\xBB\xBF"[..], "// 表\r\nint bom();\r\n".as_bytes()].concat());
    write(&repo.join("src/u16le.h"), &utf16("// 表\r\nint u16le();\r\n", true, true));
    write(&repo.join("src/u16be.h"), &utf16("// 表\r\nint u16be();\r\n", false, true));
    write(&repo.join("src/u16nobom.h"), &utf16("// 表\r\nint u16nobom();\r\n", true, false));
    // ISO-2022-JP, EUC-JP (EB A1 has no CP932 mapping), and EUC-JP that CP932 reads too.
    write(&repo.join("docs/jis.txt"), &encoding_rs::ISO_2022_JP.encode("表示\n").0);
    write(&repo.join("docs/euc.txt"), &[&b"// "[..], &[0xEB, 0xA1], b"\n"].concat());
    write(&repo.join("docs/euc_or_cp932.txt"), &encoding_rs::EUC_JP.encode("アイウ\n").0);
    // The NEC and IBM forms of Ⅰ.
    write(&repo.join("src/necibm.h"), b"// \x87\x54 \xFA\x4A\r\n");
    // UTF-8 and CP932 lines in one file, and a file that reads both ways (ý / ﾃｽ).
    write(&repo.join("src/mixed.cpp"), &[&sjis("// 表\r\n")[..], "// 表\r\nint mixed();\r\n".as_bytes()].concat());
    write(&repo.join("src/ambiguous.h"), b"// \xC3\xBD\n");
    // CR, LF and CRLF in one file.
    write(&repo.join("src/eol.h"), &[&sjis("// 表\r")[..], b"int a;\nint b;\r\nint c;"].concat());
    // A Japanese file name, and a binary file.
    write(&repo.join("src/日本語.h"), &sjis("// 日本語\r\nint nihongo();\r\n"));
    write(&repo.join("assets/logo.bin"), b"\x89PNG\x00\x00\x00\x0D\xFF\x00");
    write(&repo.join("src/Case.h"), b"int case_h();\n");
    // A path of about 950 characters with a file name of 240.
    let deep = (0..14).map(|i| format!("dir_{}_{i:02}", "d".repeat(40))).collect::<Vec<_>>().join("/");
    let long_name = format!("{}.h", "h".repeat(238));
    write(&mixenc::paths::join_rel(&repo, &format!("{deep}/{long_name}")), &sjis("// 表\r\nint in_long();\r\n"));
    // A generated header in an ignored folder that the database includes.
    write(&repo.join("out/gen.tlh"), b"int generated();\n");
    // A response file (UTF-16LE, as MSBuild writes them).
    write(
        &repo.join("src/main.rsp"),
        &utf16(&format!("/DFROM_RSP /I\"{}\"\r\n", repo.join("rspinc").display()), true, true),
    );
    fs::create_dir_all(repo.join("rspinc")).unwrap();
    write(&repo.join(".gitignore"), b"out/\ncompile_commands.json\n");

    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "core.longpaths", "true"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    git(&repo, &["add", "-A"]);
    // Not added yet: still mirrored (git lists it as untracked).
    write(&repo.join("src/new.h"), b"int not_added_yet();\n");

    // An include folder outside the repository, in CP932.
    write(&ext.join("ext_hidden.h"), &sjis("// 表\r\nint ext_hidden();\r\n"));

    // The database names the folders by their 8.3 short names where the volume has them (as on
    // GitHub's runners, whose temporary folder is under RUNNER~1); mixenc must still see them as
    // the repository's.
    let short = |p: &Path| mixenc::paths::short_form(p).display().to_string();
    let src = mixenc::paths::short_form(&repo.join("src"));
    let entry = |file: &str| {
        serde_json::json!({
            "directory": src.display().to_string(),
            "file": src.join(file).display().to_string(),
            "arguments": [
                r"C:\VS\cl.exe", "/c",
                format!("/I{}", src.display()),
                "/I", short(&ext),
                format!("/I{}", short(&repo.join("out"))),
                format!(r"/I{}\not_built_yet", short(&repo)),
                "/source-charset:shift_jis", "/execution-charset:shift_jis",
                "@main.rsp",
                file,
            ],
        })
    };
    let db = serde_json::json!([entry("traps.cpp"), entry("literal.cpp")]);
    write(&repo.join("compile_commands.json"), serde_json::to_string_pretty(&db).unwrap().as_bytes());

    let config = serde_json::json!({
        "repos": [{ "path": "repos/cloneA", "db": "repos/cloneA/compile_commands.json" }],
    });
    write(&ws.join("mixenc.json"), serde_json::to_string_pretty(&config).unwrap().as_bytes());
    Fixture { _tmp: tmp, ws, repo, ext, deep, long_name }
}

fn items(report: &doctor::Report, severity: Severity, check: &str) -> Vec<doctor::Item> {
    report.items.iter().filter(|i| i.severity == severity && i.check == check).cloned().collect()
}

#[test]
fn every_trap_is_mirrored_right() {
    let f = fixture();
    let ws = f.workspace();
    let results = sync::sync(&ws, None).unwrap();
    let r = &results[0];

    // The 0x5C trap: no line of the mirror ends with a backslash, and the lines stay one to one.
    assert_eq!(
        f.read_mirror("src/traps.cpp"),
        "#include \"hyo.h\"\r\n// 表\r\nint after_hyo();\r\n// 能\r\nint after_nou();\r\n// ソ\r\nint after_so();\r\nint main() { return hidden(1); }\r\n"
    );
    assert_eq!(f.read_mirror("src/hyo.h"), "#pragma once\r\n// 表\r\nint hidden(int);\r\n");
    assert_eq!(f.read_mirror("src/literal.cpp"), "const char* s = \"表示\";\n");
    assert_eq!(f.read_mirror("src/undefined.h"), "// \u{FFFD}\r\nint after_undefined();\r\n");
    assert_eq!(f.read_mirror("src/cut.h"), "// \u{FFFD}\r\nint after_cut();\r\n");
    for name in ["bom", "u16le", "u16be", "u16nobom"] {
        assert_eq!(f.read_mirror(&format!("src/{name}.h")), format!("// 表\r\nint {name}();\r\n"), "{name}");
    }
    assert_eq!(f.read_mirror("docs/jis.txt"), "表示\n");
    assert_eq!(f.read_mirror("src/necibm.h"), "// Ⅰ Ⅰ\r\n");
    assert_eq!(f.read_mirror("src/mixed.cpp"), "// 表\r\n// 表\r\nint mixed();\r\n");
    assert_eq!(f.read_mirror("src/ambiguous.h"), "// ý\n");
    assert_eq!(f.read_mirror("src/eol.h"), "// 表\rint a;\nint b;\r\nint c;");
    assert_eq!(f.read_mirror("src/日本語.h"), "// 日本語\r\nint nihongo();\r\n");
    assert_eq!(f.read_mirror("src/new.h"), "int not_added_yet();\n");
    assert_eq!(f.read_mirror("out/gen.tlh"), "int generated();\n");
    let long = format!("{}/{}", f.deep, f.long_name);
    assert!(f.mirror(&long).as_os_str().len() > 900);
    assert_eq!(f.read_mirror(&long), "// 表\r\nint in_long();\r\n");

    // Not mirrored: the binary file and the file EUC-JP and CP932 both read.
    assert!(!f.mirror("assets/logo.bin").exists());
    assert!(!f.mirror("docs/euc_or_cp932.txt").exists());
    assert!(!f.mirror("compile_commands.json.orig").exists());
    // Mirrors are read-only.
    assert!(fs::metadata(f.mirror("src/traps.cpp")).unwrap().permissions().readonly());

    // The include folder outside: mirrored under _ext, mapped for clangd.
    assert_eq!(r.external.len(), 1);
    let ext_mirror = &r.external[0].0.mirror;
    assert!(ext_mirror.starts_with(f.ws.join("mirror").join("_ext")));
    assert_eq!(fs::read_to_string(ext_mirror.join("ext_hidden.h")).unwrap(), "// 表\r\nint ext_hidden();\r\n");
    let mappings = &r.state.path_mappings;
    assert!(mappings.contains(&mixenc::paths::forward(&f.repo.display().to_string())), "{mappings}");
    assert!(mappings.contains(&mixenc::paths::forward(&f.ext.display().to_string())), "{mappings}");

    // The mirror database: paths moved, charsets gone, the response file written out (invariant 6).
    let db = fs::read_to_string(f.mirror("compile_commands.json")).unwrap();
    let entries: Vec<serde_json::Value> = serde_json::from_str(&db).unwrap();
    let args: Vec<&str> = entries[0]["arguments"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
    let mirror_src = f.mirror("src").display().to_string();
    assert_eq!(entries[0]["directory"], mirror_src.as_str());
    assert!(args.contains(&format!("/I{mirror_src}").as_str()), "{args:?}");
    assert!(args.contains(&ext_mirror.display().to_string().as_str()), "{args:?}");
    assert!(args.contains(&"/DFROM_RSP"), "{args:?}");
    assert!(args.contains(&format!("/I{}", f.mirror("rspinc").display()).as_str()), "{args:?}");
    assert!(!args.iter().any(|a| a.contains("charset") || a.starts_with('@')), "{args:?}");
    assert!(!db.to_lowercase().contains(&f.repo.display().to_string().to_lowercase().replace('\\', "\\\\")), "{db}");
    assert!(r.state.db_problems.is_empty(), "{:?}", r.state.db_problems);

    // The doctor: the undecided file is broken, the unusual encodings are notices.
    let report = doctor::run(&ws);
    let undecided = items(&report, Severity::Broken, "judgement");
    assert_eq!(undecided.len(), 1, "{report:#?}");
    assert_eq!(undecided[0].paths, ["repos/cloneA/docs/euc_or_cp932.txt (ambiguous-cp932-euc-jp)"]);
    let notices: Vec<String> =
        items(&report, Severity::Notice, "judgement").iter().map(|i| i.message.clone()).collect();
    for expected in [
        "mix UTF-8 and CP932",
        "read both as UTF-8 and as CP932",
        "EUC-JP",
        "ISO-2022-JP",
        "UTF-16 without a BOM",
        "read in no encoding",
    ] {
        assert!(notices.iter().any(|n| n.contains(expected)), "{expected}: {notices:?}");
    }
    let long_names = items(&report, Severity::Notice, "long-paths");
    assert!(long_names.iter().any(|i| i.paths.iter().any(|p| p.ends_with(&f.long_name))), "{long_names:?}");
    let missing = items(&report, Severity::Notice, "db");
    assert!(missing.iter().any(|i| i.paths.iter().any(|p| p.ends_with("not_built_yet"))), "{missing:?}");
    assert!(items(&report, Severity::Broken, "mirror-stale").is_empty(), "{report:#?}");
    assert!(items(&report, Severity::Broken, "mirror-integrity").is_empty(), "{report:#?}");
    assert!(items(&report, Severity::Broken, "db").is_empty(), "{report:#?}");
    assert!(report.summary.traps >= 7, "{:?}", report.summary);
}

#[test]
fn overrides_changes_and_repairs() {
    let f = fixture();
    let ws = f.workspace();
    sync::sync(&ws, None).unwrap();

    // A person decides the ambiguous file.
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(f.ws.join("mixenc.json")).unwrap()).unwrap();
    config["overrides"] = serde_json::json!({
        "repos/cloneA/docs/euc_or_cp932.txt": { "as": "euc-jp", "judged": "ambiguous-cp932-euc-jp" },
        "repos/cloneA/assets/logo.bin": { "as": "skip" },
    });
    fs::write(f.ws.join("mixenc.json"), serde_json::to_string_pretty(&config).unwrap()).unwrap();
    let ws = f.workspace();
    sync::sync(&ws, None).unwrap();
    assert_eq!(f.read_mirror("docs/euc_or_cp932.txt"), "アイウ\n");
    let report = doctor::run(&ws);
    assert!(items(&report, Severity::Broken, "judgement").is_empty(), "{report:#?}");
    assert!(items(&report, Severity::Notice, "overrides").is_empty(), "{report:#?}");

    // An original changes: the doctor says the mirror is behind (invariant 8), a sync catches up.
    write(&f.repo.join("src/hyo.h"), &sjis("#pragma once\r\n// 表\r\nint hidden(int, int);\r\n"));
    let report = doctor::run(&ws);
    assert_eq!(items(&report, Severity::Broken, "mirror-stale")[0].paths, ["repos/cloneA/src/hyo.h"]);
    let r = sync::sync(&ws, None).unwrap();
    assert_eq!(r[0].repo.written, 1);
    assert!(f.read_mirror("src/hyo.h").contains("int hidden(int, int);"));

    // A mirror changed by hand: the doctor sees it (invariant 2), a sync writes it again.
    let target = f.mirror("src/literal.cpp");
    mixenc::fsx::set_read_only(&target, false).unwrap();
    fs::write(&target, "edited by hand\n").unwrap();
    let report = doctor::run(&ws);
    assert!(items(&report, Severity::Broken, "mirror-integrity")[0].paths[0].contains("literal.cpp"), "{report:#?}");
    let r = sync::sync(&ws, None).unwrap();
    assert_eq!(r[0].repo.repaired, 1);
    assert_eq!(f.read_mirror("src/literal.cpp"), "const char* s = \"表示\";\n");

    // A file only in the mirror is removed; a removed original takes its mirror with it.
    write(&f.mirror("src/stray.h"), b"stray\n");
    fs::remove_file(f.repo.join("src/cut.h")).unwrap();
    let r = sync::sync(&ws, None).unwrap();
    assert_eq!(r[0].repo.removed, 2);
    assert!(!f.mirror("src/stray.h").exists() && !f.mirror("src/cut.h").exists());

    // A rename that changes only the case keeps the file.
    git(&f.repo, &["mv", "src/Case.h", "src/case.h"]);
    sync::sync(&ws, None).unwrap();
    assert_eq!(f.read_mirror("src/case.h"), "int case_h();\n");

    // Nothing changed: nothing is written.
    let r = sync::sync(&ws, None).unwrap();
    assert_eq!((r[0].repo.written, r[0].repo.removed, r[0].repo.repaired), (0, 0, 0));
    let report = doctor::run(&ws);
    let broken: Vec<_> = items(&report, Severity::Broken, "mirror-stale")
        .into_iter()
        .chain(items(&report, Severity::Broken, "mirror-integrity"))
        .collect();
    assert!(broken.is_empty(), "{broken:#?}");
}

#[test]
fn the_command_line() {
    let f = fixture();
    let exe = env!("CARGO_BIN_EXE_mixenc");
    let run = |args: &[&str]| Command::new(exe).args(args).current_dir(&f.ws).output().unwrap();

    let out = run(&["sync"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let out = run(&["--json", "doctor"]);
    assert_eq!(out.status.code(), Some(1), "the undecided file is broken");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["format_version"], 1);
    assert!(report["summary"]["broken"].as_u64().unwrap() >= 1);
    let out = run(&["judge", "repos/cloneA/src/traps.cpp", "repos/cloneA/src/bom.h"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("cp932\t") && text.contains("(3 lines a UTF-8 reader would splice)"), "{text}");
    assert!(text.contains("utf8-bom\t"), "{text}");
    let out = run(&["doctor"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("Broken") && text.contains("fix:"), "{text}");
}
