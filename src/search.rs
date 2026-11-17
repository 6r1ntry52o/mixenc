//! Searching the mirrors: a regular expression over the UTF-8 mirror of each file, so that a CP932
//! original matches `表示` as its text says. The analysis tool `search` and the Copilot hook that
//! completes grep's answer use it.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rayon::prelude::*;
use regex::{Regex, RegexBuilder};

use crate::index::Index;
use crate::judge::Encoding;
use crate::mirror::{self, Root};
use crate::text;

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub ignore_case: bool,
    /// The pattern is plain text, not a regular expression.
    pub fixed: bool,
    /// Globs the file must match (a `!` before one excludes): `*.cpp`, `src/**/*.h`, `*.{h,cpp}`.
    pub globs: Vec<String>,
    /// ripgrep's file types (`cpp`, `c`, `h`); an unknown one filters nothing.
    pub types: Vec<String>,
}

pub struct Matcher {
    regex: Regex,
    include: Vec<Regex>,
    exclude: Vec<Regex>,
}

impl Matcher {
    pub fn new(pattern: &str, opts: &Options) -> Result<Matcher> {
        let source = if opts.fixed { regex::escape(pattern) } else { pattern.to_string() };
        let regex = RegexBuilder::new(&source)
            .case_insensitive(opts.ignore_case)
            .build()
            .with_context(|| format!("the pattern {pattern:?} is not a regular expression this search reads"))?;
        let mut include = Vec::new();
        let mut exclude = Vec::new();
        for glob in &opts.globs {
            match glob.strip_prefix('!') {
                Some(g) => exclude.push(glob_regex(g)?),
                None => include.push(glob_regex(glob)?),
            }
        }
        let typed: Vec<&str> = opts.types.iter().filter_map(|t| type_glob(t)).collect();
        if !typed.is_empty() {
            include.push(glob_regex(&format!("*.{{{}}}", typed.join(",")))?);
        }
        Ok(Matcher { regex, include, exclude })
    }

    /// Whether a file (its path relative to where the search starts, with `/`) is searched.
    pub fn wants(&self, rel: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|g| g.is_match(rel)))
            && !self.exclude.iter().any(|g| g.is_match(rel))
    }

    /// The lines of `text` that match: (1-based line, the line without its line break).
    pub fn lines(&self, bytes: &[u8]) -> Vec<(usize, String)> {
        let Ok(s) = std::str::from_utf8(bytes) else { return Vec::new() };
        if !self.regex.is_match(s) {
            return Vec::new();
        }
        text::split_lines(bytes)
            .into_iter()
            .enumerate()
            .filter_map(|(i, (content, _))| {
                let line = std::str::from_utf8(content).ok()?;
                self.regex.is_match(line).then(|| (i + 1, line.to_string()))
            })
            .collect()
    }
}

fn type_glob(t: &str) -> Option<&'static str> {
    Some(match t.to_ascii_lowercase().as_str() {
        "cpp" | "c++" | "cxx" => "C,H,cc,cpp,cxx,c++,h,hh,hpp,hxx,inl,ipp,tpp",
        "c" => "c,h,H",
        "h" => "h,hh,hpp,hxx,inl",
        _ => return None,
    })
}

/// A glob as a regular expression over a relative path with `/`. A glob without `/` matches a name
/// at any depth, as in `.gitignore`; letters match in either case, as Windows names do.
pub fn glob_regex(glob: &str) -> Result<Regex> {
    let g = glob.replace('\\', "/");
    let g = g.trim_start_matches("./");
    let mut re = String::from("(?i)^");
    if !g.contains('/') {
        re.push_str("(?:.*/)?");
    }
    let chars: Vec<char> = g.chars().collect();
    let mut braces = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    re.push_str(".*");
                    i += 2;
                }
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '{' => {
                braces += 1;
                re.push_str("(?:");
            }
            '}' if braces > 0 => {
                braces -= 1;
                re.push(')');
            }
            ',' if braces > 0 => re.push('|'),
            '[' => {
                let end = chars[i + 1..].iter().position(|&c| c == ']').map(|p| i + 1 + p);
                match end {
                    Some(end) => {
                        let class: String = chars[i + 1..end].iter().collect();
                        let class = class.strip_prefix('!').map_or(class.clone(), |c| format!("^{c}"));
                        re.push('[');
                        re.push_str(&class.replace('\\', "\\\\"));
                        re.push(']');
                        i = end + 1;
                        continue;
                    }
                    None => re.push_str(&regex::escape("[")),
                }
            }
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push('$');
    Regex::new(&re).with_context(|| format!("the glob {glob:?} is not one this search reads"))
}

/// The files of a root that have a mirror, from its index: (path relative to the root, judgement).
pub fn mirrored(root: &Root) -> Vec<(String, Encoding)> {
    Index::load(&root.mirror, &root.source)
        .files
        .into_iter()
        .filter(|(rel, s)| s.mirror.is_some() && !mirror::is_reserved(rel))
        .map(|(rel, s)| (rel, s.read_as.unwrap_or(s.judged)))
        .collect()
}

/// A matching line of a file of a root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub rel: String,
    pub line: usize,
    pub text: String,
}

/// Searches the mirrors of `files` (relative to the root), in parallel; hits in path order.
pub fn search(root: &Root, files: &[String], m: &Matcher) -> Vec<Hit> {
    let mut hits: Vec<Hit> = files
        .par_iter()
        .flat_map_iter(|rel| {
            let bytes = fs::read(crate::paths::join_rel(&root.mirror, rel)).unwrap_or_default();
            m.lines(&bytes).into_iter().map(|(line, text)| Hit { rel: rel.clone(), line, text }).collect::<Vec<_>>()
        })
        .collect();
    hits.sort_by(|a, b| a.rel.cmp(&b.rel).then(a.line.cmp(&b.line)));
    hits
}

/// The lines of a mirror file (for context lines around a hit).
pub fn file_lines(path: &Path) -> Vec<String> {
    let bytes = fs::read(path).unwrap_or_default();
    text::split_lines(&bytes).into_iter().map(|(content, _)| String::from_utf8_lossy(content).into_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let m = |g: &str, p: &str| glob_regex(g).unwrap().is_match(p);
        assert!(m("*.cpp", "src/a.cpp") && m("*.cpp", "a.CPP") && !m("*.cpp", "a.cpp.bak"));
        assert!(m("src/*.h", "src/a.h") && !m("src/*.h", "src/x/a.h") && !m("src/*.h", "lib/src/a.h"));
        assert!(m("src/**/*.h", "src/x/y/a.h") && m("src/**/*.h", "src/a.h"));
        assert!(m("*.{h,cpp}", "x/a.h") && m("*.{h,cpp}", "a.cpp") && !m("*.{h,cpp}", "a.c"));
        assert!(m("[ab].c", "a.c") && !m("[!ab].c", "a.c") && m("?.c", "z.c"));
    }

    #[test]
    fn matches_lines_and_filters_files() {
        let opts = Options { globs: vec!["*.cpp".into(), "!gen/**".into()], ..Options::default() };
        let m = Matcher::new("表示", &opts).unwrap();
        assert!(m.wants("src/a.cpp") && !m.wants("src/a.h") && !m.wants("gen/x.cpp"));
        let text = "int a;\r\n// 表示する\r\nint b; // 表示\n".as_bytes();
        assert_eq!(m.lines(text), vec![(2, "// 表示する".to_string()), (3, "int b; // 表示".to_string())]);
        let fixed = Matcher::new("a(", &Options { fixed: true, ..Options::default() }).unwrap();
        assert_eq!(fixed.lines(b"x\na(1)\n"), vec![(2, "a(1)".to_string())]);
        let typed = Matcher::new("x", &Options { types: vec!["cpp".into()], ..Options::default() }).unwrap();
        assert!(typed.wants("a.hpp") && !typed.wants("a.rc"));
        assert!(Matcher::new("(", &Options::default()).is_err());
    }
}
