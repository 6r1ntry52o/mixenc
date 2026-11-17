//! The mirror database: lazymsbuild's `compile_commands.json` with every path into a mirrored root
//! moved to its mirror, response files written out into the arguments (their paths too), and the
//! charset options taken out (clang reads UTF-8 only). Invariant 6 is checked on the result: no
//! path in it leads to an original, and no charset option is left.
//!
//! A relative path stays relative when it stays inside the root of the entry's directory (the
//! directory moves to the mirror with it); one that leaves that root is made absolute, in the
//! mirror if it lands in another mirrored root.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::mirror::Root;
use crate::{cmdline, cp932, paths, text};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub directory: String,
    pub file: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Reads a database; entries with `command` get `arguments` instead.
pub fn read(path: &Path) -> Result<Vec<Entry>> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", paths::display(path)))?;
    let text = std::str::from_utf8(bytes.strip_prefix(text::UTF8_BOM).unwrap_or(&bytes))
        .with_context(|| format!("{} is not UTF-8", paths::display(path)))?;
    let mut entries: Vec<Entry> =
        serde_json::from_str(text).with_context(|| format!("reading {}", paths::display(path)))?;
    for e in &mut entries {
        if e.arguments.is_empty()
            && let Some(command) = e.command.take()
        {
            e.arguments = cmdline::split(&command);
        }
    }
    Ok(entries)
}

/// Original roots and their mirrors, the most specific first (as clangd's `--path-mappings`
/// takes the first that matches).
#[derive(Clone, Debug)]
pub struct Mapper {
    roots: Vec<(String, String)>,
}

impl Mapper {
    pub fn new(roots: &[Root]) -> Mapper {
        let mut roots: Vec<(String, String)> = roots
            .iter()
            .map(|r| (paths::long_form(&r.source.to_string_lossy()), paths::long_form(&r.mirror.to_string_lossy())))
            .collect();
        roots.sort_by_key(|(source, _)| std::cmp::Reverse(source.len()));
        Mapper { roots }
    }

    /// The index of the root `abs` is in.
    pub fn root_of(&self, abs: &str) -> Option<usize> {
        let abs = paths::long_form(abs);
        self.roots.iter().position(|(source, _)| paths::is_under(&abs, source))
    }

    /// `abs` moved to its mirror, if it is in a root.
    pub fn map(&self, abs: &str) -> Option<String> {
        // A database may name a folder by its 8.3 short name (RUNNER~1); the roots have long names.
        let abs = paths::long_form(abs);
        self.roots.iter().find_map(|(source, mirror)| {
            paths::rest_under(&abs, source)
                .map(|rest| if rest.is_empty() { mirror.clone() } else { format!("{mirror}\\{rest}") })
        })
    }

    /// clangd's `--path-mappings` value. clangd matches a path's drive letter in its case, and
    /// editors differ (VS Code sends `c:`, a database has `C:`), so every root comes twice: with an
    /// upper-case and a lower-case drive letter. With only `C:`, a file the editor opens is not moved
    /// to the mirror and clangd reads the originals of the headers next to it; with only `c:`, the
    /// answers from the index come back as mirror paths (measured with clangd 23).
    pub fn path_mappings(&self) -> String {
        let mut pairs = Vec::new();
        for (source, mirror) in &self.roots {
            let (source, mirror) = (paths::forward(source), paths::forward(mirror));
            pairs.push(format!("{}={}", drive_case(&source, true), drive_case(&mirror, true)));
            if has_drive(&source) || has_drive(&mirror) {
                pairs.push(format!("{}={}", drive_case(&source, false), drive_case(&mirror, false)));
            }
        }
        pairs.join(",")
    }
}

fn has_drive(p: &str) -> bool {
    p.as_bytes().get(1) == Some(&b':')
}

fn drive_case(p: &str, upper: bool) -> String {
    if !has_drive(p) {
        return p.to_string();
    }
    let drive = if upper { p[..1].to_ascii_uppercase() } else { p[..1].to_ascii_lowercase() };
    format!("{drive}{}", &p[1..])
}

/// Options whose value is a path, longest first, and whether the value may be the next argument.
const PATH_OPTIONS: &[(&str, bool)] = &[
    ("sourceDependencies", true),
    ("external:I", true),
    ("idirafter", true),
    ("isystem", true),
    ("include", true),
    ("iquote", true),
    ("imsvc", true),
    ("FI", true),
    ("FU", true),
    ("AI", true),
    ("Fo", false),
    ("Fd", false),
    ("Fp", false),
    ("Fa", false),
    ("Fe", false),
    ("Fi", false),
    ("Fm", false),
    ("FR", false),
    ("Fr", false),
    ("Yc", false),
    ("Yu", false),
    ("Tp", true),
    ("Tc", true),
    ("I", true),
];
/// Path options that name an include folder.
const INCLUDE_OPTIONS: &[&str] = &["external:I", "idirafter", "isystem", "iquote", "imsvc", "I"];
/// Options whose value is not a path but may be the next argument (so it is not taken for a file).
const VALUE_OPTIONS: &[&str] = &["D", "U", "Xclang", "x", "target", "o"];
const CHARSET_OPTIONS: &[&str] = &[
    "/source-charset:",
    "-source-charset:",
    "/execution-charset:",
    "-execution-charset:",
    "-finput-charset=",
    "-fexec-charset=",
];

/// One argument, seen as mixenc needs it.
#[derive(Debug)]
enum Arg<'a> {
    /// Kept as it is (with a value that is not a path, if it takes the next argument).
    Other(&'a str),
    /// A path, alone (a source file) or as the value of an option, joined to it or as the next
    /// argument.
    Path {
        option: Option<&'a str>,
        prefix: &'a str,
        value: &'a str,
        separate: bool,
    },
    Charset(&'a str),
    /// `/link` and everything after it.
    Link(&'a [String]),
}

fn parse(args: &[String]) -> Vec<Arg<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        if i == 1 {
            // The compiler.
            out.push(Arg::Other(arg));
            continue;
        }
        if arg == "/link" || arg == "-link" {
            out.push(Arg::Link(&args[i - 1..]));
            break;
        }
        if CHARSET_OPTIONS.iter().any(|c| arg.starts_with(c)) {
            out.push(Arg::Charset(arg));
            continue;
        }
        let Some(name) = arg.strip_prefix('/').or_else(|| arg.strip_prefix('-')).filter(|n| !n.is_empty()) else {
            out.push(Arg::Path { option: None, prefix: "", value: arg, separate: false });
            continue;
        };
        if let Some(&(option, separate_ok)) = PATH_OPTIONS.iter().find(|(o, _)| name.starts_with(o)) {
            let joined = &name[option.len()..];
            let colon = usize::from(joined.starts_with(':') && option != "external:I");
            let value = &joined[colon..];
            let prefix = &arg[..1 + option.len() + colon];
            if !value.is_empty() {
                out.push(Arg::Path { option: Some(option), prefix, value, separate: false });
            } else if separate_ok && i < args.len() {
                out.push(Arg::Path { option: Some(option), prefix: arg, value: &args[i], separate: true });
                i += 1;
            } else {
                out.push(Arg::Other(arg));
            }
            continue;
        }
        out.push(Arg::Other(arg));
        if VALUE_OPTIONS.contains(&name) && i < args.len() {
            out.push(Arg::Other(&args[i]));
            i += 1;
        }
    }
    out
}

/// Response files (`@file`, relative to `directory`) written out into the arguments.
/// Whether the entry compiles its sources as UTF-8 (`/utf-8` or `/source-charset:utf-8`, in its
/// arguments or its response files).
pub fn compiles_as_utf8(e: &Entry) -> bool {
    let directory = paths::long_form(&e.directory);
    expand_response_files(&e.arguments, &directory, &mut Vec::new()).iter().any(|arg| {
        let arg = arg.to_ascii_lowercase();
        matches!(arg.strip_prefix(['/', '-']), Some("utf-8" | "source-charset:utf-8" | "source-charset:.65001"))
    })
}

fn expand_response_files(args: &[String], directory: &str, problems: &mut Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for arg in args {
        let Some(file) = arg.strip_prefix('@') else {
            out.push(arg.clone());
            continue;
        };
        let path = paths::absolute(directory, file);
        match fs::read(&path) {
            Ok(bytes) => out.extend(cmdline::split(&decode_response_file(&bytes))),
            Err(err) => {
                problems.push(format!("response file {path}: {err}"));
                out.push(arg.clone());
            }
        }
    }
    out
}

fn decode_response_file(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return crate::judge::utf16(rest, true).unwrap_or_default();
    }
    let bytes = bytes.strip_prefix(text::UTF8_BOM).unwrap_or(bytes);
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut out = String::new();
    for (line, eol) in text::split_lines(bytes) {
        cp932::decode_line(line, &mut out);
        out.push_str(std::str::from_utf8(eol).unwrap_or_default());
    }
    out
}

/// A path value moved to the mirror (see the module's note on relative paths).
fn move_path(value: &str, directory: &str, mapper: &Mapper) -> String {
    let abs = paths::absolute(directory, value);
    let relative = !paths::is_absolute(value) && !value.starts_with('\\') && !value.starts_with('/');
    if relative && mapper.root_of(&abs).is_some() && mapper.root_of(&abs) == mapper.root_of(directory) {
        return value.to_string();
    }
    match mapper.map(&abs) {
        Some(moved) => moved,
        None if relative && mapper.root_of(directory).is_some() => abs,
        None => value.to_string(),
    }
}

/// The mirror database of `entries`, and what could not be done.
pub fn rewrite(entries: &[Entry], mapper: &Mapper) -> (Vec<Entry>, Vec<String>) {
    let mut problems = Vec::new();
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let directory = paths::normalize(&e.directory);
        let args = expand_response_files(&e.arguments, &directory, &mut problems);
        let mut arguments = Vec::with_capacity(args.len());
        for arg in parse(&args) {
            match arg {
                Arg::Other(a) => arguments.push(a.to_string()),
                Arg::Charset(_) => {}
                Arg::Link(rest) => arguments.extend(rest.iter().cloned()),
                Arg::Path { prefix, value, separate, .. } => {
                    let moved = move_path(value, &directory, mapper);
                    if separate {
                        arguments.push(prefix.to_string());
                        arguments.push(moved);
                    } else {
                        arguments.push(format!("{prefix}{moved}"));
                    }
                }
            }
        }
        out.push(Entry {
            directory: mapper.map(&directory).unwrap_or(directory.clone()),
            file: move_path(&e.file, &directory, mapper),
            arguments,
            command: None,
            output: e.output.as_deref().map(|o| move_path(o, &directory, mapper)),
        });
    }
    (out, problems)
}

/// Invariant 6 on a mirror database: what in it still leads to an original, or is a charset option.
pub fn check(entries: &[Entry], mapper: &Mapper) -> Vec<String> {
    let mut violations = Vec::new();
    for e in entries {
        let directory = paths::normalize(&e.directory);
        let mut paths_in_entry: Vec<(&str, &str)> = vec![("directory", &e.directory), ("file", &e.file)];
        if let Some(output) = &e.output {
            paths_in_entry.push(("output", output));
        }
        for arg in parse(&e.arguments) {
            match arg {
                Arg::Path { prefix, value, .. } => {
                    paths_in_entry.push((if prefix.is_empty() { "argument" } else { prefix }, value))
                }
                Arg::Charset(a) => violations.push(format!("{}: charset option {a}", e.file)),
                Arg::Other(a) if a.starts_with('@') => paths_in_entry.push(("response file", &a[1..])),
                _ => {}
            }
        }
        for (what, value) in paths_in_entry {
            if mapper.root_of(&paths::absolute(&directory, value)).is_some() {
                violations.push(format!("{}: {what} leads to the original: {value}", e.file));
            }
        }
    }
    violations
}

/// What the original database says about the files around the repository.
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    /// Include folders outside the repository, not the toolchain's (to mirror).
    pub external_dirs: Vec<String>,
    /// The toolchain's include folders (Visual Studio, Windows Kits, LLVM).
    pub toolchain_dirs: Vec<String>,
    /// Include folders that do not exist (often a build makes them).
    pub missing_dirs: Vec<String>,
    /// Files in the repository the database names (sources, forced includes) and headers right in
    /// its include folders inside the repository: relative paths with `/`. Those git does not list
    /// are generated, and are mirrored too.
    pub repo_files: BTreeSet<String>,
    /// Sources and forced includes the database names that do not exist.
    pub missing_files: Vec<String>,
}

pub fn analyze(entries: &[Entry], repo: &Path) -> Analysis {
    let repo = paths::normalize(&repo.to_string_lossy());
    let mut include_dirs = BTreeSet::new();
    let mut files = BTreeSet::new();
    for e in entries {
        let directory = paths::long_form(&e.directory);
        files.insert(paths::long_form(&paths::absolute(&directory, &e.file)));
        let mut ignored = Vec::new();
        let args = expand_response_files(&e.arguments, &directory, &mut ignored);
        for arg in parse(&args) {
            if let Arg::Path { option: Some(option), value, .. } = arg {
                let abs = paths::long_form(&paths::absolute(&directory, value));
                if INCLUDE_OPTIONS.contains(&option) {
                    include_dirs.insert(abs);
                } else if option == "FI" {
                    files.insert(abs);
                }
            }
        }
    }
    let mut a = Analysis::default();
    let mut seen_keys = BTreeSet::new();
    for dir in include_dirs {
        if !seen_keys.insert(paths::key(&dir)) {
            continue;
        }
        let in_repo = paths::is_under(&dir, &repo);
        if !Path::new(&dir).is_dir() {
            a.missing_dirs.push(dir);
        } else if in_repo {
            for name in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = name.path();
                if path.is_file()
                    && is_header(&path)
                    && let Some(rest) = paths::rest_under(&path.to_string_lossy(), &repo)
                {
                    a.repo_files.insert(rest.replace('\\', "/"));
                }
            }
        } else if is_toolchain(&dir) {
            a.toolchain_dirs.push(dir);
        } else {
            a.external_dirs.push(dir);
        }
    }
    for file in files {
        match paths::rest_under(&file, &repo) {
            Some(rest) if Path::new(&file).is_file() => {
                a.repo_files.insert(rest.replace('\\', "/"));
            }
            Some(_) => a.missing_files.push(file),
            None => {}
        }
    }
    a.external_dirs = outermost(a.external_dirs);
    a
}

/// The folders not inside another of them.
pub fn outermost(mut dirs: Vec<String>) -> Vec<String> {
    dirs.sort_by_key(|d| paths::key(d));
    let mut kept: Vec<String> = Vec::new();
    for d in dirs {
        if !kept.iter().any(|k| paths::is_under(&d, k)) {
            kept.push(d);
        }
    }
    kept
}

/// Whether an include folder belongs to the compiler or the SDK (not mirrored; the doctor checks
/// its headers are UTF-8 instead).
pub fn is_toolchain(dir: &str) -> bool {
    let key = paths::key(dir) + "\\";
    ["\\microsoft visual studio\\", "\\vc\\tools\\msvc\\", "\\windows kits\\", "\\microsoft sdks\\", "\\llvm\\"]
        .iter()
        .any(|s| key.contains(s))
}

fn is_header(path: &Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "h" | "hh" | "hpp" | "hxx" | "h++" | "inl" | "ipp" | "tlh" | "tli" | "inc")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mapper() -> Mapper {
        Mapper::new(&[
            Root { source: PathBuf::from(r"C:\ws\repos\a"), mirror: PathBuf::from(r"C:\ws\mirror\a") },
            Root { source: PathBuf::from(r"C:\Vendor\inc"), mirror: PathBuf::from(r"C:\ws\mirror\_ext\inc-12345678") },
        ])
    }

    fn entry(args: &[&str]) -> Entry {
        Entry {
            directory: r"C:\ws\repos\a\src".into(),
            file: r"C:\ws\repos\a\src\main.cpp".into(),
            arguments: args.iter().map(|s| s.to_string()).collect(),
            command: None,
            output: None,
        }
    }

    #[test]
    fn moves_paths_and_drops_charsets() {
        let e = entry(&[
            r"C:\VS\cl.exe",
            "--target=x86_64-pc-windows-msvc",
            r"/IC:\ws\repos\a\inc",
            "/I",
            r"..\..\..\..\Vendor\inc",
            "/Iinclude",
            r"/external:IC:\Vendor\inc\sub",
            r"/FIC:\ws\repos\a\pch.h",
            "/Yupch.h",
            r"/IC:\Program Files\Windows Kits\10\Include\um",
            "/source-charset:shift_jis",
            "/execution-charset:shift_jis",
            "/utf-8",
            "/D",
            "A=1",
            "/c",
            r"C:\ws\repos\a\src\main.cpp",
        ]);
        let (out, problems) = rewrite(&[e], &mapper());
        assert!(problems.is_empty());
        let e = &out[0];
        assert_eq!(e.directory, r"C:\ws\mirror\a\src");
        assert_eq!(e.file, r"C:\ws\mirror\a\src\main.cpp");
        assert_eq!(
            e.arguments,
            [
                r"C:\VS\cl.exe",
                "--target=x86_64-pc-windows-msvc",
                r"/IC:\ws\mirror\a\inc",
                "/I",
                r"C:\ws\mirror\_ext\inc-12345678",
                "/Iinclude",
                r"/external:IC:\ws\mirror\_ext\inc-12345678\sub",
                r"/FIC:\ws\mirror\a\pch.h",
                "/Yupch.h",
                r"/IC:\Program Files\Windows Kits\10\Include\um",
                "/utf-8",
                "/D",
                "A=1",
                "/c",
                r"C:\ws\mirror\a\src\main.cpp",
            ]
        );
        assert_eq!(check(&out, &mapper()), Vec::<String>::new());
    }

    #[test]
    fn the_check_finds_originals_and_charsets() {
        let e = entry(&["cl.exe", r"/IC:\ws\repos\a\inc", "/source-charset:utf-8", "main.cpp"]);
        let violations = check(&[e], &mapper());
        // directory, file, /I, the charset and main.cpp (relative to the original directory)
        assert_eq!(violations.len(), 5, "{violations:?}");
    }

    #[test]
    fn response_files_are_written_out() {
        let dir = tempfile::tempdir().unwrap();
        let rsp = dir.path().join("cl.rsp");
        let text = format!("/I\"{}\\inc\"\r\n/DX", dir.path().display());
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        fs::write(&rsp, utf16).unwrap();
        let m = Mapper::new(&[Root { source: dir.path().to_path_buf(), mirror: PathBuf::from(r"C:\m") }]);
        let e = Entry {
            directory: dir.path().display().to_string(),
            file: "a.cpp".into(),
            arguments: vec!["cl.exe".into(), "@cl.rsp".into(), "a.cpp".into()],
            command: None,
            output: None,
        };
        let (out, problems) = rewrite(&[e], &m);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(out[0].arguments, ["cl.exe", r"/IC:\m\inc", "/DX", "a.cpp"]);
        assert_eq!(out[0].directory, r"C:\m");
    }

    #[test]
    fn path_mappings_most_specific_first() {
        let m = Mapper::new(&[
            Root { source: PathBuf::from(r"C:\ws\repos\a"), mirror: PathBuf::from(r"C:\ws\mirror\a") },
            Root {
                source: PathBuf::from(r"C:\ws\repos\a\third_party\x"),
                mirror: PathBuf::from(r"C:\ws\mirror\_ext\x"),
            },
        ]);
        assert_eq!(
            m.path_mappings(),
            "C:/ws/repos/a/third_party/x=C:/ws/mirror/_ext/x,c:/ws/repos/a/third_party/x=c:/ws/mirror/_ext/x,C:/ws/repos/a=C:/ws/mirror/a,c:/ws/repos/a=c:/ws/mirror/a"
        );
        assert!(is_toolchain(r"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.40\include"));
        assert!(!is_toolchain(r"C:\Program Files\Vendor\include"));
        assert_eq!(outermost(vec![r"C:\a\b".into(), r"C:\a".into(), r"C:\ab".into()]), [r"C:\a", r"C:\ab"]);
    }
}
