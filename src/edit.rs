//! The front gate: the one way an AI writes an original. It replaces text in a file of a workspace
//! (or writes a new file) and writes the file back in its own encoding: only the lines whose text
//! changed are encoded, every other line keeps its bytes. Before anything is written, the result
//! must read back as the text asked for and pass the commit gate's check ([`crate::gate`]), and
//! the original must be as it was when it was read. The mirror is brought up to date after.
//!
//! - A mixed file's changed lines keep their line's encoding; its new lines get the encoding most
//!   of its lines are in.
//! - A file with no non-ASCII text so far gets the encoding the files around it use
//!   ([`crate::defaults`]), or the one the caller says.
//! - A character the encoding has no form for (`😀`, `𠮷` in CP932) stops the edit, with the line.
//! - A path into a mirror stands for its original.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use similar::{Algorithm, DiffOp};

use crate::config::{ReadAs, RepoPaths, Workspace};
use crate::defaults::{Choice, Defaults, Pick};
use crate::doc::{self, Doc, Eol, Line, LineEnc};
use crate::judge::{self, Encoding};
use crate::mirror::{self, Root};
use crate::{fsx, gate, paths, text};

/// The encodings a caller may ask for (for a file with no non-ASCII text so far).
pub const ASKABLE: [Encoding; 3] = [Encoding::Cp932, Encoding::Utf8Bom, Encoding::Utf8];

/// What an edit will write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub bytes: Vec<u8>,
    /// The file's encoding after the edit.
    pub encoding: Encoding,
    /// The lines written (1-based, in the new file), if any.
    pub lines: Option<(usize, usize)>,
    /// Lines of the old file not in the new one.
    pub removed: usize,
    pub notes: Vec<String>,
}

/// Replaces the one place `old` is in the text of `bytes` with `new`. Line breaks in both may be
/// `\n`; the file's own are kept, and new lines get the one most of its lines have.
pub fn plan_replace(
    bytes: &[u8],
    read_as: Option<ReadAs>,
    old: &str,
    new: &str,
    asked: Option<Encoding>,
    pick: &mut dyn FnMut() -> Pick,
) -> Result<Plan> {
    let judged = judge::judge(bytes);
    let read = match mirror::wanted_read_as(judged, read_as) {
        Some(e) => e,
        None if read_as == Some(ReadAs::Skip) => {
            bail!("mixenc.json leaves this file out (\"skip\"); it is not edited here")
        }
        None if judged == Encoding::Binary => bail!("the file is binary (NUL bytes that are not UTF-16)"),
        None => bail!("the file reads both as CP932 and as EUC-JP; decide which in mixenc.json (overrides) first"),
    };
    let doc = doc::parse(bytes, read).context("the file cannot be read as lines")?;
    let text = doc.normalized();
    let (old, new) = (lf(old), lf(new));
    if old.is_empty() {
        bail!("old_string is empty: give the text to replace (create writes a new file)");
    }
    let found: Vec<usize> = text.match_indices(&old).map(|(i, _)| i).collect();
    match found.len() {
        1 => {}
        0 => bail!(
            "old_string is not in the file; copy it exactly from the file as read right (its mirror is UTF-8 with the same lines). Line breaks may be \\n"
        ),
        n => {
            let lines: Vec<String> =
                found.iter().take(5).map(|&i| (text[..i].matches('\n').count() + 1).to_string()).collect();
            bail!("old_string is in the file {n} times (lines {}); give more of the text around it", lines.join(", "))
        }
    }
    let replaced = format!("{}{}{}", &text[..found[0]], new, &text[found[0] + old.len()..]);
    let wanted: Vec<(String, bool)> = split_lf(&replaced);

    let non_ascii = !replaced.is_ascii();
    let mut notes = Vec::new();
    let target = match (read, asked) {
        (Encoding::Ascii, Some(e)) => askable(e)?,
        (Encoding::Ascii, None) if non_ascii => {
            let pick = pick();
            match pick.choice {
                Choice::Write(e) => {
                    notes.push(format!("the file had only ASCII text; it is {e} now ({})", pick.why));
                    e
                }
                Choice::Ask => bail!(ask(&pick)),
            }
        }
        (Encoding::Ascii, None) => Encoding::Ascii,
        (read, Some(e)) if e != read => {
            bail!("the file is {read} and stays so (encoding is only for a file with no non-ASCII text so far)")
        }
        (read, _) => read,
    };
    let bom =
        if read == Encoding::Ascii && target == Encoding::Utf8Bom { text::UTF8_BOM.to_vec() } else { doc.bom.clone() };
    let built = build(&doc, &wanted, target)?;
    if read == Encoding::AmbiguousUtf8Cp932 && built.written.iter().any(|&j| !wanted[j].0.is_ascii()) {
        bail!(
            "the file reads both as UTF-8 and as CP932; decide which in mixenc.json (overrides) before writing non-ASCII text into it"
        );
    }
    let result = Doc { bom, lines: built.lines }.to_bytes();
    verify(Some(bytes), &result, read_as, target, &replaced, pick)?;
    notes.extend(undecided_note(&result, read_as));
    Ok(Plan {
        encoding: if target == Encoding::Ascii { judge::judge(&result) } else { target },
        bytes: result,
        lines: built.written.first().map(|&first| (first + 1, built.written.last().copied().unwrap_or(first) + 1)),
        removed: built.removed,
        notes,
    })
}

/// A new file of `content` (its line breaks as given).
pub fn plan_create(content: &str, asked: Option<Encoding>, pick: &mut dyn FnMut() -> Pick) -> Result<Plan> {
    let mut notes = Vec::new();
    let target = match asked {
        Some(e) => askable(e)?,
        None if content.is_ascii() => Encoding::Ascii,
        None => {
            let pick = pick();
            match pick.choice {
                Choice::Write(e) => {
                    notes.push(format!("written in {e} ({})", pick.why));
                    e
                }
                Choice::Ask => bail!(ask(&pick)),
            }
        }
    };
    let enc = line_enc(target).unwrap_or(LineEnc::Cp932);
    let mut bytes = if target == Encoding::Utf8Bom { text::UTF8_BOM.to_vec() } else { Vec::new() };
    let mut unwritable = Vec::new();
    let lines = text::split_lines(content.as_bytes());
    for (i, (line, eol)) in lines.iter().enumerate() {
        let line = std::str::from_utf8(line).unwrap_or_default();
        let eol = match *eol {
            b"\r\n" => Eol::CrLf,
            b"\r" => Eol::Cr,
            b"\n" => Eol::Lf,
            _ => Eol::None,
        };
        match doc::encode_line(line, eol, enc) {
            Ok(raw) => bytes.extend(raw),
            Err(chars) => unwritable.extend(chars.into_iter().map(|c| (c, i + 1))),
        }
    }
    if !unwritable.is_empty() {
        bail!(unwritable_message(&unwritable, enc));
    }
    verify(None, &bytes, None, target, &lf(content), pick)?;
    notes.extend(undecided_note(&bytes, None));
    Ok(Plan {
        encoding: if target == Encoding::Ascii { judge::judge(&bytes) } else { target },
        lines: (!lines.is_empty()).then_some((1, lines.len())),
        bytes,
        removed: 0,
        notes,
    })
}

struct Built {
    lines: Vec<Line>,
    /// Indexes (in `wanted`) of the lines encoded anew.
    written: Vec<usize>,
    removed: usize,
}

/// The new lines: those whose text is the same as an old line's (by a line diff) are that line's
/// bytes; the others are encoded.
fn build(doc: &Doc, wanted: &[(String, bool)], target: Encoding) -> Result<Built> {
    let old_keys: Vec<(&str, bool)> = doc.lines.iter().map(gate::key).collect();
    let new_keys: Vec<(&str, bool)> = wanted.iter().map(|(t, e)| (t.as_str(), *e)).collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    let ops = similar::capture_diff_slices_deadline(Algorithm::Myers, &old_keys, &new_keys, Some(deadline));
    let style = doc.eol_style();
    let major = doc.major();
    let file_enc = line_enc(target);
    let mut built = Built { lines: Vec::new(), written: Vec::new(), removed: 0 };
    let mut unwritable = Vec::new();
    let mut new_line = |built: &mut Built, j: usize, paired: Option<&Line>| {
        let (text, has_eol) = &wanted[j];
        let eol = match (has_eol, paired) {
            (false, _) => Eol::None,
            (true, Some(p)) if p.eol != Eol::None => p.eol,
            (true, _) => style,
        };
        // A mixed file: a changed line stays in its line's encoding, a new one gets the major one.
        let enc = file_enc.unwrap_or(match paired {
            Some(p) if p.ok && !p.text.is_ascii() => p.enc,
            _ => major,
        });
        match doc::encode_line(text, eol, enc) {
            Ok(raw) => built.lines.push(Line { raw, text: text.clone(), eol, enc, ok: true }),
            Err(chars) => unwritable.extend(chars.into_iter().map(|c| (c, j + 1, enc))),
        }
        built.written.push(j);
    };
    for op in ops {
        match op {
            DiffOp::Equal { old_index, len, .. } => {
                built.lines.extend_from_slice(&doc.lines[old_index..old_index + len])
            }
            DiffOp::Delete { old_len, .. } => built.removed += old_len,
            DiffOp::Insert { new_index, new_len, .. } => {
                for j in new_index..new_index + new_len {
                    new_line(&mut built, j, None);
                }
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                for k in 0..new_len {
                    new_line(&mut built, new_index + k, doc.lines.get(old_index + k).filter(|_| k < old_len));
                }
                built.removed += old_len.saturating_sub(new_len);
            }
        }
    }
    if let Some(&(_, _, enc)) = unwritable.first() {
        let chars: Vec<(char, usize)> = unwritable.iter().map(|&(c, line, _)| (c, line)).collect();
        bail!(unwritable_message(&chars, enc));
    }
    Ok(built)
}

/// The checks on what is about to be written: it reads back as the text asked for, and the commit
/// gate would let it through.
fn verify(
    old: Option<&[u8]>,
    new: &[u8],
    read_as: Option<ReadAs>,
    target: Encoding,
    wanted: &str,
    pick: &mut dyn FnMut() -> Pick,
) -> Result<()> {
    let read = match target {
        Encoding::Mixed | Encoding::Broken => Encoding::Mixed,
        Encoding::Ascii => judge::judge(new),
        e => e,
    };
    let back = doc::parse(new, read).map(|d| d.normalized());
    if back.as_deref() != Some(wanted) {
        bail!(
            "the result would not read back as the text given (in {target}, as the mirror reads it); nothing was written"
        );
    }
    let check = gate::check(old, new, read_as, pick);
    if !check.passed() {
        let findings: Vec<String> = check
            .findings
            .iter()
            .map(|f| match f.line {
                Some(line) => format!("line {line}: {}", f.message),
                None => f.message.clone(),
            })
            .collect();
        bail!("the commit gate would stop the result ({}); nothing was written", findings.join("; "));
    }
    Ok(())
}

fn undecided_note(bytes: &[u8], read_as: Option<ReadAs>) -> Option<String> {
    (read_as.is_none() && judge::judge(bytes) == Encoding::AmbiguousCp932EucJp).then(|| {
        "the file now reads both as CP932 and as EUC-JP, so it is not mirrored until mixenc.json says which (overrides: \"as\": \"cp932\")".to_string()
    })
}

fn line_enc(e: Encoding) -> Option<LineEnc> {
    match e {
        Encoding::Ascii | Encoding::Utf8 | Encoding::Utf8Bom | Encoding::AmbiguousUtf8Cp932 => Some(LineEnc::Utf8),
        Encoding::Cp932 => Some(LineEnc::Cp932),
        Encoding::EucJp => Some(LineEnc::EucJp),
        Encoding::Iso2022jp => Some(LineEnc::Iso2022jp),
        Encoding::Utf16le | Encoding::Utf16leNobom => Some(LineEnc::Utf16le),
        Encoding::Utf16be | Encoding::Utf16beNobom => Some(LineEnc::Utf16be),
        // Line by line.
        Encoding::Mixed | Encoding::Broken => None,
        Encoding::AmbiguousCp932EucJp | Encoding::Binary => Some(LineEnc::Cp932),
    }
}

fn askable(e: Encoding) -> Result<Encoding> {
    if ASKABLE.contains(&e) {
        Ok(e)
    } else {
        bail!("encoding {e} is not one to write a file in here (cp932, utf8-bom or utf8)")
    }
}

fn ask(pick: &Pick) -> String {
    format!(
        "the files around are mostly UTF-8 without a BOM ({}), but MSVC reads such files as CP932 unless the project compiles with /utf-8. Ask the user which encoding to write this file in (cp932, utf8-bom or utf8), then call again with encoding set; nothing was written",
        pick.why
    )
}

fn unwritable_message(chars: &[(char, usize)], enc: LineEnc) -> String {
    if chars.is_empty() {
        return format!("the text cannot be written in {}; nothing was written", enc.name());
    }
    let listed: Vec<String> = chars
        .iter()
        .map(|&(c, line)| {
            let near = match enc {
                LineEnc::Cp932 => {
                    doc::cp932_nearest(c).map(|n| format!(" ({} has {n} U+{:04X})", enc.name(), n as u32))
                }
                _ => None,
            };
            format!("{c} U+{:04X} on line {line}{}", c as u32, near.unwrap_or_default())
        })
        .collect();
    format!("{} has no form for these characters, so nothing was written: {}", enc.name(), listed.join("; "))
}

/// The text with every line break as `\n`.
fn lf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Lines of a `\n` text, each with whether a line break ends it.
fn split_lf(s: &str) -> Vec<(String, bool)> {
    let mut lines: Vec<(String, bool)> = s.split('\n').map(|l| (l.to_string(), true)).collect();
    if let Some(last) = lines.pop()
        && !last.0.is_empty()
    {
        lines.push((last.0, false));
    }
    lines
}

/// What an edit of a workspace's file did.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// The original written.
    pub path: PathBuf,
    pub encoding: Encoding,
    pub lines: Option<(usize, usize)>,
    pub removed: usize,
    pub notes: Vec<String>,
}

impl Outcome {
    pub fn summary(&self) -> String {
        let mut out = format!("wrote {} ({})", paths::display(&self.path), self.encoding);
        match self.lines {
            Some((a, b)) if a == b => out.push_str(&format!(": line {a}")),
            Some((a, b)) => out.push_str(&format!(": lines {a}-{b}")),
            None => {}
        }
        if self.removed > 0 {
            out.push_str(&format!(", {} lines removed", self.removed));
        }
        out.push_str("; every other line kept its bytes");
        for note in &self.notes {
            out.push_str(&format!("\n- {note}"));
        }
        out
    }
}

/// A file of a repository of a workspace.
struct Target {
    ws: Workspace,
    repo: RepoPaths,
    abs: PathBuf,
    /// Relative to the repository, with `/`.
    rel: String,
}

fn locate(path: &Path) -> Result<Target> {
    let mut abs = paths::canonical(&std::path::absolute(path)?);
    let ws = Workspace::find(abs.parent().unwrap_or(&abs))?;
    let repos = ws.repos();
    for repo in &repos {
        if let Some(rest) = paths::rest_under(&abs.to_string_lossy(), &repo.mirror.to_string_lossy()) {
            let rel = rest.replace('\\', "/");
            if rel.is_empty() || mirror::is_reserved(&rel) {
                bail!("{} is mixenc's own file in the mirror, not a mirrored one", paths::display(&abs));
            }
            abs = paths::canonical(&paths::join_rel(&repo.source, &rel));
        }
    }
    if paths::is_under(&abs.to_string_lossy(), &ws.external_root().to_string_lossy()) {
        bail!(
            "{} is the mirror of an include folder outside the repositories; mixenc edits files of the repositories only",
            paths::display(&abs)
        );
    }
    let found = repos
        .into_iter()
        .find_map(|r| paths::rest_under(&abs.to_string_lossy(), &r.source.to_string_lossy()).map(|rest| (r, rest)));
    match found {
        Some((repo, rest)) if !rest.is_empty() => Ok(Target { rel: rest.replace('\\', "/"), repo, abs, ws }),
        _ => bail!("{} is not in a repository of the workspace {}", paths::display(&abs), paths::display(&ws.root)),
    }
}

impl Target {
    fn read_as(&self) -> Option<ReadAs> {
        self.ws.override_for(&self.abs).map(|o| o.read_as)
    }

    /// Mirrors the file after it was written.
    fn mirror(&self) -> Option<String> {
        let root = Root { source: self.repo.source.clone(), mirror: self.repo.mirror.clone() };
        let read_as = |abs: &Path| self.ws.override_for(abs).map(|o| o.read_as);
        match mirror::sync_files(&root, std::slice::from_ref(&self.rel), &read_as) {
            Ok(outcome) => {
                outcome.not_mirrored.first().map(|(_, problem)| format!("the mirror was not written: {problem:?}"))
            }
            Err(err) => Some(format!("the mirror was not updated: {err:#} (mixenc sync)")),
        }
    }
}

/// Replaces `old` with `new` in a file of a workspace (the original, or its mirror's path).
pub fn replace(path: &Path, old: &str, new: &str, asked: Option<Encoding>) -> Result<Outcome> {
    let t = locate(path)?;
    let display = paths::display(&t.abs);
    let bytes = match fs::read(&t.abs) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("{display} does not exist (create writes a new file)")
        }
        Err(err) => return Err(err).with_context(|| format!("reading {display}")),
    };
    if fs::metadata(&t.abs)?.permissions().readonly() {
        bail!("{display} is read-only; nothing was written");
    }
    let mut cached = None;
    let mut pick = || cached.get_or_insert_with(|| Defaults::load(&t.ws, &t.repo).for_file(&t.rel)).clone();
    let plan = plan_replace(&bytes, t.read_as(), old, new, asked, &mut pick)?;
    if fs::read(&t.abs).with_context(|| format!("reading {display}"))? != bytes {
        bail!("{display} changed while it was being edited; read it again");
    }
    let dir = t.abs.parent().unwrap_or(&t.abs).to_path_buf();
    fsx::write_replace(&dir, &t.abs, &plan.bytes, false).with_context(|| format!("writing {display}"))?;
    let mut notes = plan.notes;
    notes.extend(t.mirror());
    Ok(Outcome { path: t.abs.clone(), encoding: plan.encoding, lines: plan.lines, removed: plan.removed, notes })
}

/// Writes a new file of a workspace (the original's path, or its mirror's).
pub fn create(path: &Path, content: &str, asked: Option<Encoding>) -> Result<Outcome> {
    let t = locate(path)?;
    let display = paths::display(&t.abs);
    if fs::symlink_metadata(&t.abs).is_ok() {
        bail!("{display} exists (edit changes it)");
    }
    let mut cached = None;
    let mut pick = || cached.get_or_insert_with(|| Defaults::load(&t.ws, &t.repo).for_file(&t.rel)).clone();
    let plan = plan_create(content, asked, &mut pick)?;
    if let Some(dir) = t.abs.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", paths::display(dir)))?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&t.abs)
        .with_context(|| format!("creating {display}"))?;
    file.write_all(&plan.bytes).with_context(|| format!("writing {display}"))?;
    drop(file);
    let mut notes = plan.notes;
    notes.extend(t.mirror());
    Ok(Outcome { path: t.abs.clone(), encoding: plan.encoding, lines: plan.lines, removed: 0, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sjis(s: &str) -> Vec<u8> {
        crate::win::cp932_encode(s).unwrap()
    }

    fn replace(bytes: &[u8], old: &str, new: &str) -> Result<Plan> {
        plan_replace(bytes, None, old, new, None, &mut Pick::fallback)
    }

    #[test]
    fn only_the_changed_lines_are_encoded() {
        // Line 3 has Ⅰ in its IBM form (FA 4A), which Windows would write as NEC's (87 54).
        let original = [&sjis("// 表示\r\nint a;\r\n")[..], b"// \xFA\x4A\r\n", &sjis("int b; // 能\r\n")].concat();
        let plan = replace(&original, "int a;", "int a = 1; // Ⅰ").unwrap();
        let expected =
            [&sjis("// 表示\r\n")[..], b"int a = 1; // \x87\x54\r\n", b"// \xFA\x4A\r\n", &sjis("int b; // 能\r\n")]
                .concat();
        assert_eq!(plan.bytes, expected);
        assert_eq!((plan.encoding, plan.lines, plan.removed), (Encoding::Cp932, Some((2, 2)), 0));
    }

    #[test]
    fn line_breaks_follow_the_file() {
        let original = sjis("a\r\nb\r\nc\r\n");
        let plan = replace(&original, "b\n", "b\n// 表\n").unwrap();
        assert_eq!(plan.bytes, sjis("a\r\nb\r\n// 表\r\nc\r\n"));
        assert_eq!(plan.lines, Some((3, 3)));
        let removed = replace(&original, "b\r\nc\r\n", "").unwrap();
        assert_eq!((removed.bytes, removed.lines, removed.removed), (b"a\r\n".to_vec(), None, 2));
    }

    #[test]
    fn stops_where_cp932_has_no_character() {
        let err = replace(&sjis("// 表\n"), "表", "表😀〜").unwrap_err().to_string();
        assert!(err.contains("😀 U+1F600 on line 1"), "{err}");
        assert!(err.contains("〜 U+301C on line 1 (CP932 has ～ U+FF5E)"), "{err}");
    }

    #[test]
    fn finds_the_text_once() {
        let original = b"int a;\nint a;\n";
        assert!(replace(original, "int a;", "x").unwrap_err().to_string().contains("2 times (lines 1, 2)"));
        assert!(replace(original, "int b;", "x").unwrap_err().to_string().contains("not in the file"));
        assert!(replace(original, "", "x").is_err());
    }

    #[test]
    fn a_mixed_file_keeps_each_line_in_its_encoding() {
        let original = [&sjis("// 表\n")[..], "// 能\n".as_bytes(), &sjis("// ソ\n")].concat();
        let plan = replace(&original, "// 能\n// ソ\n", "// 能です\n// ソです\n// 新\n").unwrap();
        let expected = [&sjis("// 表\n")[..], "// 能です\n".as_bytes(), &sjis("// ソです\n// 新\n")].concat();
        assert_eq!(plan.bytes, expected);
        assert_eq!(plan.encoding, Encoding::Mixed);
    }

    #[test]
    fn an_ascii_file_takes_the_default() {
        let plan = replace(b"int a;\n", "int a;", "int a; // 表").unwrap();
        assert_eq!(plan.bytes, sjis("int a; // 表\n"));
        assert_eq!(plan.encoding, Encoding::Cp932);
        assert!(plan.notes[0].contains("it is cp932 now"), "{:?}", plan.notes);
        let bom =
            plan_replace(b"int a;\n", None, "int a;", "int a; // 表", Some(Encoding::Utf8Bom), &mut Pick::fallback)
                .unwrap();
        assert_eq!(bom.bytes, [text::UTF8_BOM, "int a; // 表\n".as_bytes()].concat());
        let ask = Pick { choice: Choice::Ask, why: "2 of 2 ...".into(), utf8_flag: false };
        let err = plan_replace(b"int a;\n", None, "int a;", "// 表", None, &mut || ask.clone()).unwrap_err();
        assert!(err.to_string().contains("Ask the user"), "{err}");
        // A file with non-ASCII text keeps its encoding whatever is asked.
        assert!(plan_replace(&sjis("// 表\n"), None, "表", "能", Some(Encoding::Utf8), &mut Pick::fallback).is_err());
    }

    #[test]
    fn utf16_keeps_its_other_lines() {
        let le = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
        let original = [&[0xFF, 0xFE][..], &le("STRINGTABLE\r\nBEGIN\r\n  1 \"表\"\r\nEND\r\n")].concat();
        let plan = replace(&original, "  1 \"表\"", "  1 \"表\"\n  2 \"能\"").unwrap();
        assert_eq!(
            plan.bytes,
            [&[0xFF, 0xFE][..], &le("STRINGTABLE\r\nBEGIN\r\n  1 \"表\"\r\n  2 \"能\"\r\nEND\r\n")].concat()
        );
    }

    #[test]
    fn create_follows_the_default() {
        let plan = plan_create("// 表\nint a;\n", None, &mut Pick::fallback).unwrap();
        assert_eq!(plan.bytes, sjis("// 表\nint a;\n"));
        assert_eq!(plan.encoding, Encoding::Cp932);
        assert_eq!(plan_create("int a;\n", None, &mut Pick::fallback).unwrap().encoding, Encoding::Ascii);
        // Asked for UTF-8 without a BOM where MSVC would read it as CP932: the commit gate stops it.
        assert!(plan_create("// 表\n", Some(Encoding::Utf8), &mut Pick::fallback).is_err());
    }

    #[test]
    fn skipped_and_undecided_files_are_not_edited() {
        assert!(plan_replace(b"a", Some(ReadAs::Skip), "a", "b", None, &mut Pick::fallback).is_err());
        let euc = encoding_rs::EUC_JP.encode("// アイウ\n").0.into_owned();
        assert!(replace(&euc, "//", "/*").unwrap_err().to_string().contains("decide which"));
        let forced =
            plan_replace(&euc, Some(ReadAs::Encoding(Encoding::EucJp)), "// ", "/// ", None, &mut Pick::fallback);
        assert_eq!(forced.unwrap().bytes, encoding_rs::EUC_JP.encode("/// アイウ\n").0.into_owned());
    }
}
