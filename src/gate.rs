//! The gate's check: a file's new bytes against its old ones. The commit gate (pre-commit) runs it
//! on every staged file, and the edit tools on what they are about to write, so that they never
//! write what the commit gate would stop.
//!
//! - The encoding stays: a cp932 file does not become utf8. A file with no non-ASCII text so far
//!   may get any encoding MSVC reads right (UTF-8 without a BOM only where the project compiles
//!   with `/utf-8`, or where the files around are UTF-8 too: see [`crate::defaults`]).
//! - A line whose text did not change keeps its bytes and its line break (invariant 1). Saving a
//!   CP932 file through Unicode can change them: `Ⅰ` has an NEC form and an IBM form.
//! - A mixed or broken file gets no more lines of the encoding it has fewer of, and no more lines
//!   that read in no encoding.

use std::time::{Duration, Instant};

use similar::{Algorithm, DiffOp};

use crate::config::ReadAs;
use crate::defaults::{Choice, Pick};
use crate::doc::{self, Doc, Eol, Line};
use crate::judge::{self, Encoding};
use crate::text;

/// Lines listed per file for one kind of finding; the rest are counted.
pub const LINES_LISTED: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The line it is about (1-based, in the new file), if one.
    pub line: Option<usize>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub old: Option<Encoding>,
    pub new: Encoding,
    pub findings: Vec<Finding>,
    /// Lines whose bytes changed beyond those listed.
    pub more_lines: usize,
}

impl Check {
    pub fn passed(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Checks `new` against `old` (`None` for a new file). `read_as` is the override for the file;
/// `pick` gives the default encoding where one is needed.
pub fn check(old: Option<&[u8]>, new: &[u8], read_as: Option<ReadAs>, pick: &mut dyn FnMut() -> Pick) -> Check {
    let n = judge::judge(new);
    let mut check = Check { old: None, new: n, findings: Vec::new(), more_lines: 0 };
    if read_as == Some(ReadAs::Skip) {
        return check;
    }
    let Some(old) = old else {
        if n != Encoding::Binary {
            let pick = pick();
            if !fits(n, &pick) {
                check.add(None, misfit(n, &pick));
            }
        }
        return check;
    };
    let o = judge::judge(old);
    check.old = Some(o);
    let forced = match read_as {
        Some(ReadAs::Encoding(e)) => Some(e),
        _ => None,
    };
    if o == Encoding::Binary && forced.is_none() {
        return check;
    }
    if n == Encoding::Binary {
        check.add(None, "the file now has NUL bytes that are not UTF-16 (binary)".into());
        return check;
    }

    if o == Encoding::Ascii && forced.is_none() {
        if n != Encoding::Ascii {
            let pick = pick();
            if !fits(n, &pick) {
                check.add(None, misfit(n, &pick));
            }
        }
        // Every byte of an ASCII file changes in UTF-16.
        if !matches!(n, Encoding::Utf16le | Encoding::Utf16be) {
            check.lines(old, Encoding::Ascii, new, reading(n));
        }
        return check;
    }

    let possible = forced.map_or_else(|| readings(o), |e| vec![e]);
    let was = forced.map_or_else(|| o.name().to_string(), |e| format!("{e} (as the settings file says)"));
    let mixed = possible.iter().any(|e| matches!(e, Encoding::Mixed | Encoding::Broken));
    let accepted = if mixed {
        matches!(
            n,
            Encoding::Mixed
                | Encoding::Broken
                | Encoding::Ascii
                | Encoding::Utf8
                | Encoding::Utf8Bom
                | Encoding::Cp932
                | Encoding::AmbiguousUtf8Cp932
        )
    } else if n == Encoding::Ascii {
        possible.iter().any(|e| ascii_compatible(*e))
    } else {
        possible.iter().any(|e| readings(n).contains(e))
    };
    if !accepted {
        check.add(None, format!("the encoding changed: {was} -> {n}"));
    }
    if mixed {
        let bom = |b: &[u8]| b.starts_with(text::UTF8_BOM);
        if bom(old) != bom(new) {
            check.add(None, format!("the BOM was {}", if bom(old) { "removed" } else { "added" }));
        }
        let counts = |b: &[u8]| judge::line_counts(b.strip_prefix(text::UTF8_BOM).unwrap_or(b));
        let (a, b) = (counts(old), counts(new));
        if b.neither > a.neither {
            check.add(None, format!("more lines that read in no encoding: {} -> {}", a.neither, b.neither));
        }
        if b.minority() > a.minority() {
            check.add(
                None,
                format!(
                    "more lines in the encoding this mixed file has fewer of: {} -> {} (UTF-8 {} and CP932 {} lines now)",
                    a.minority(),
                    b.minority(),
                    b.utf8,
                    b.cp932
                ),
            );
        }
        check.lines(old, Encoding::Mixed, new, Encoding::Mixed);
        return check;
    }
    // Read both the same way where they can be, so that a line's text compares.
    let common = possible
        .iter()
        .copied()
        .find(|e| if n == Encoding::Ascii { ascii_compatible(*e) } else { readings(n).contains(e) });
    match common {
        Some(e) => check.lines(old, reading(e), new, reading(e)),
        None => check.lines(old, reading(possible[0]), new, reading(n)),
    }
    check
}

impl Check {
    fn add(&mut self, line: Option<usize>, message: String) {
        self.findings.push(Finding { line, message });
    }

    /// Lines whose text is the same in both and whose bytes are not.
    fn lines(&mut self, old: &[u8], old_read: Encoding, new: &[u8], new_read: Encoding) {
        let (Some(a), Some(b)) = (doc::parse(old, old_read), doc::parse(new, new_read)) else { return };
        let changed = same_text_other_bytes(&a, &b);
        for (line, message) in changed.iter().take(LINES_LISTED) {
            self.add(Some(*line), message.clone());
        }
        self.more_lines += changed.len().saturating_sub(LINES_LISTED);
    }
}

/// The lines (1-based in `new`) whose text and line break presence match a line of `old` in a
/// line diff, and whose bytes differ, with what changed.
pub fn same_text_other_bytes(old: &Doc, new: &Doc) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (i, j) in equal_lines(old, new) {
        let (o, n) = (&old.lines[i], &new.lines[j]);
        if o.raw != n.raw {
            found.push((j + 1, describe(o, n)));
        }
    }
    found
}

/// The pairs of lines (indexes in `old` and `new`) a line diff finds equal by text.
pub fn equal_lines(old: &Doc, new: &Doc) -> Vec<(usize, usize)> {
    let a: Vec<(&str, bool)> = old.lines.iter().map(key).collect();
    let b: Vec<(&str, bool)> = new.lines.iter().map(key).collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut pairs = Vec::new();
    for op in similar::capture_diff_slices_deadline(Algorithm::Myers, &a, &b, Some(deadline)) {
        if let DiffOp::Equal { old_index, new_index, len } = op {
            pairs.extend((0..len).map(|k| (old_index + k, new_index + k)));
        }
    }
    pairs
}

/// What a line diff compares: the text, and whether a line break ends it.
pub fn key(l: &Line) -> (&str, bool) {
    (l.text.as_str(), l.eol != Eol::None)
}

fn describe(old: &Line, new: &Line) -> String {
    if old.enc == new.enc && old.content() == new.content() {
        return format!("the line break changed: {} -> {}", old.eol.name(), new.eol.name());
    }
    if let (Some(a), Some(b)) = (old.char_bytes(), new.char_bytes()) {
        for ((c, ra), (_, rb)) in a.iter().zip(&b) {
            let (x, y) = (&old.content()[ra.clone()], &new.content()[rb.clone()]);
            if x != y {
                return format!("the text is the same but its bytes changed: {c} {} -> {}", hex(x), hex(y));
            }
        }
    }
    if old.enc != new.enc {
        format!("the text is the same but it is in {} now (was {})", new.enc.name(), old.enc.name())
    } else {
        "the text is the same but its bytes changed".into()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

/// The encodings a file judged so may really be in.
fn readings(e: Encoding) -> Vec<Encoding> {
    match e {
        Encoding::AmbiguousUtf8Cp932 => vec![Encoding::Utf8, Encoding::Cp932],
        Encoding::AmbiguousCp932EucJp => vec![Encoding::Cp932, Encoding::EucJp],
        Encoding::Binary => Vec::new(),
        e => vec![e],
    }
}

/// How a file of that encoding is read to compare lines.
fn reading(e: Encoding) -> Encoding {
    match e {
        Encoding::AmbiguousUtf8Cp932 => Encoding::Utf8,
        Encoding::AmbiguousCp932EucJp => Encoding::Cp932,
        e => e,
    }
}

/// Whether ASCII text is the same bytes in it.
fn ascii_compatible(e: Encoding) -> bool {
    matches!(
        e,
        Encoding::Ascii
            | Encoding::Utf8
            | Encoding::Cp932
            | Encoding::EucJp
            | Encoding::Iso2022jp
            | Encoding::Mixed
            | Encoding::Broken
            | Encoding::AmbiguousUtf8Cp932
            | Encoding::AmbiguousCp932EucJp
    )
}

/// Whether a file with non-ASCII text in `n` is one MSVC reads right where `pick` applies.
pub fn fits(n: Encoding, pick: &Pick) -> bool {
    n == Encoding::Ascii
        || readings(n).iter().any(|e| match e {
            Encoding::Cp932 | Encoding::Utf8Bom | Encoding::Utf16le | Encoding::Utf16be => true,
            Encoding::Utf8 => pick.allows_utf8(),
            _ => false,
        })
}

fn misfit(n: Encoding, pick: &Pick) -> String {
    let why = match n {
        Encoding::Utf8 => "MSVC reads UTF-8 without a BOM as CP932 unless the project compiles with /utf-8",
        Encoding::Mixed => "UTF-8 and CP932 lines in one file",
        Encoding::Broken => "bytes that read in no encoding",
        _ => "MSVC does not read it",
    };
    let want = match pick.choice {
        Choice::Write(e) => e.name().to_string(),
        Choice::Ask => "cp932 or utf8-bom".to_string(),
    };
    format!("non-ASCII text in {n} ({why}); write it in {want} ({})", pick.why)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sjis(s: &str) -> Vec<u8> {
        crate::win::cp932_encode(s).unwrap()
    }

    fn run(old: Option<&[u8]>, new: &[u8]) -> Check {
        check(old, new, None, &mut Pick::fallback)
    }

    #[test]
    fn an_edit_that_changes_only_its_lines_passes() {
        let old = sjis("// 表示\r\nint a;\r\n// Ⅰ\r\n");
        let new = sjis("// 表示する\r\nint a;\r\nint b;\r\n// Ⅰ\r\n");
        assert!(run(Some(&old), &new).passed());
    }

    #[test]
    fn cp932_saved_as_utf8_is_stopped() {
        let old = sjis("// 表示\nint a;\n");
        let check = run(Some(&old), "// 表示\nint a;\n".as_bytes());
        assert_eq!(check.findings[0].message, "the encoding changed: cp932 -> utf8");
        assert_eq!(check.findings[1].line, Some(1));
        assert!(check.findings[1].message.contains("表 95 5C -> E8 A1 A8"), "{:?}", check.findings[1]);
    }

    #[test]
    fn an_untouched_line_whose_bytes_changed_is_stopped() {
        // Ⅰ written in its IBM form (FA 4A) by an editor that read and wrote the file through Unicode.
        let old = [&b"// \x87\x54\r\n"[..], &sjis("// 表\r\n")].concat();
        let new = [&b"// \xFA\x4A\r\n"[..], &sjis("// 表です\r\n")].concat();
        let check = run(Some(&old), &new);
        assert_eq!(
            check.findings,
            [Finding {
                line: Some(1), message: "the text is the same but its bytes changed: Ⅰ 87 54 -> FA 4A".into()
            }]
        );
    }

    #[test]
    fn line_breaks_count_as_bytes() {
        let check = run(Some(b"a\r\nb\r\n"), b"a\nb\r\nc\r\n");
        assert_eq!(check.findings, [Finding { line: Some(1), message: "the line break changed: CRLF -> LF".into() }]);
        let many = run(Some(b"a\r\nb\r\nc\r\nd\r\ne\r\n"), b"a\nb\nc\nd\ne\n");
        assert_eq!((many.findings.len(), many.more_lines), (LINES_LISTED, 2));
    }

    #[test]
    fn an_ascii_file_gets_what_msvc_reads() {
        let old = b"int a;\n";
        let utf8 = run(Some(old), "int a; // 表\n".as_bytes());
        assert!(utf8.findings[0].message.starts_with("non-ASCII text in utf8 (MSVC reads"), "{:?}", utf8.findings);
        assert!(run(Some(old), &sjis("int a; // 表\n")).passed());
        assert!(run(Some(old), &[text::UTF8_BOM, "int a; // 表\n".as_bytes()].concat()).passed());
        let with_flag = Pick { utf8_flag: true, ..Pick::fallback() };
        assert!(check(Some(old), "int a; // 表\n".as_bytes(), None, &mut || with_flag.clone()).passed());
        // A new file the same way.
        assert!(!run(None, "// 表\n".as_bytes()).passed());
        assert!(run(None, &sjis("// 表\n")).passed());
        assert!(run(None, b"\x00\x01\x02binary\xFF").passed());
    }

    #[test]
    fn a_mixed_file_gets_no_more_of_its_lesser_encoding() {
        let old = [&sjis("// 表\n// 表\n")[..], "// 表\n".as_bytes()].concat();
        let more_cp932 = [&old[..], &sjis("// 能\n")].concat();
        assert!(run(Some(&old), &more_cp932).passed());
        let more_utf8 = [&old[..], "// 能\n".as_bytes()].concat();
        assert!(run(Some(&old), &more_utf8).findings[0].message.starts_with("more lines in the encoding"));
        // Made into one encoding: the encoding is fine, but the lines whose bytes changed are listed.
        assert!(run(Some(&old), &sjis("// 表\n// 表\n// 表\n")).findings.iter().all(|f| f.line.is_some()));
        let utf8 = run(Some(sjis("// 表\n").as_slice()), &[&sjis("// 表\n")[..], "// 表\n".as_bytes()].concat());
        assert_eq!(utf8.findings[0].message, "the encoding changed: cp932 -> mixed");
    }

    #[test]
    fn overrides_and_binaries() {
        let skip = check(Some(b"a"), "表".as_bytes(), Some(ReadAs::Skip), &mut Pick::fallback);
        assert!(skip.passed());
        assert!(run(Some(b"\x00\x01\xFF"), b"\x00\x02\xFF").passed());
        assert!(!run(Some(b"text\n"), b"\x00\x01\xFF").passed());
        // A file mixenc.json reads as cp932 stays cp932.
        let euc = encoding_rs::EUC_JP.encode("// アイウ\n").0.into_owned();
        let forced =
            check(Some(&euc), "// アイウ\n".as_bytes(), Some(ReadAs::Encoding(Encoding::Cp932)), &mut Pick::fallback);
        assert_eq!(forced.findings[0].message, "the encoding changed: cp932 (as the settings file says) -> utf8");
    }
}
