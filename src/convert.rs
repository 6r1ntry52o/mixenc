//! Converting an original to its mirror (UTF-8, the same lines and line breaks), and checking the
//! result before it is written:
//! - invariant 3: the mirror has the original's lines, one to one, and a line of the mirror ends
//!   with a backslash exactly when the original line ends with a real one (not the second byte of
//!   a CP932 character);
//! - invariant 4: the mirror is strict UTF-8.

use crate::judge::{self, Encoding};
use crate::{cp932, text};

/// The mirror of `bytes` read as `encoding`, or `None` for an encoding that is not read until a
/// person decides (ambiguous between CP932 and EUC-JP, binary).
pub fn to_utf8(bytes: &[u8], encoding: Encoding) -> Option<Vec<u8>> {
    let mirror = match encoding {
        Encoding::Ascii | Encoding::Utf8 | Encoding::AmbiguousUtf8Cp932 => by_lines(bytes, Reader::Utf8),
        Encoding::Utf8Bom => by_lines(bytes.strip_prefix(text::UTF8_BOM).unwrap_or(bytes), Reader::Utf8),
        Encoding::Cp932 => by_lines(bytes, Reader::Cp932),
        Encoding::Mixed | Encoding::Broken => {
            by_lines(bytes.strip_prefix(text::UTF8_BOM).unwrap_or(bytes), Reader::Utf8ElseCp932)
        }
        Encoding::Utf16le => utf16_lossy(bytes.strip_prefix(b"\xFF\xFE").unwrap_or(bytes), true),
        Encoding::Utf16be => utf16_lossy(bytes.strip_prefix(b"\xFE\xFF").unwrap_or(bytes), false),
        Encoding::Utf16leNobom => utf16_lossy(bytes, true),
        Encoding::Utf16beNobom => utf16_lossy(bytes, false),
        // Neither decoder can take a backslash into a character: in EUC-JP every byte of a
        // character is 0x8E or above, and ISO-2022-JP is decoded as a whole, with its shifts.
        Encoding::EucJp => encoding_rs::EUC_JP.decode_without_bom_handling(bytes).0.into_owned(),
        Encoding::Iso2022jp => encoding_rs::ISO_2022_JP.decode_without_bom_handling(bytes).0.into_owned(),
        Encoding::AmbiguousCp932EucJp | Encoding::Binary => return None,
    };
    Some(mirror.into_bytes())
}

#[derive(Clone, Copy)]
enum Reader {
    /// Strict UTF-8, U+FFFD for what is not (only when an override says UTF-8 for a file that is not).
    Utf8,
    Cp932,
    /// UTF-8 where the line reads so, else CP932.
    Utf8ElseCp932,
}

fn by_lines(bytes: &[u8], reader: Reader) -> String {
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 2);
    for (line, eol) in text::split_lines(bytes) {
        match (reader, std::str::from_utf8(line)) {
            (Reader::Utf8 | Reader::Utf8ElseCp932, Ok(s)) => out.push_str(s),
            (Reader::Utf8, Err(_)) => out.push_str(&String::from_utf8_lossy(line)),
            (Reader::Cp932 | Reader::Utf8ElseCp932, _) => {
                cp932::decode_line(line, &mut out);
            }
        }
        // The line break is ASCII.
        out.push_str(std::str::from_utf8(eol).unwrap_or_default());
    }
    out
}

fn utf16_lossy(bytes: &[u8], little_endian: bool) -> String {
    let mut out: String =
        char::decode_utf16(judge::utf16_units(bytes, little_endian)).map(|c| c.unwrap_or(cp932::REPLACEMENT)).collect();
    if !bytes.len().is_multiple_of(2) {
        out.push(cp932::REPLACEMENT);
    }
    out
}

/// The lines of `bytes`, read as `encoding`, that a UTF-8 reader would splice with the next one.
pub fn traps(bytes: &[u8], encoding: Encoding) -> u32 {
    let lines = match encoding {
        Encoding::Cp932 => text::split_lines(bytes),
        Encoding::Mixed | Encoding::Broken => text::split_lines(bytes.strip_prefix(text::UTF8_BOM).unwrap_or(bytes)),
        _ => return 0,
    };
    let cp932_lines =
        lines.iter().filter(|(line, _)| encoding == Encoding::Cp932 || std::str::from_utf8(line).is_err());
    cp932_lines.filter(|(line, _)| cp932::is_trap(line)).count() as u32
}

/// Why a mirror may not be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    NotUtf8,
    LineCount { original: usize, mirror: usize },
    LineBreak { line: usize },
    Backslash { line: usize, original: bool },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::NotUtf8 => write!(f, "the mirror is not strict UTF-8"),
            Violation::LineCount { original, mirror } => {
                write!(f, "the mirror has {mirror} lines where the original has {original}")
            }
            Violation::LineBreak { line } => {
                write!(f, "line {line}: the mirror's line break differs from the original's")
            }
            Violation::Backslash { line, original: true } => {
                write!(f, "line {line}: the original ends with a backslash and the mirror does not")
            }
            Violation::Backslash { line, original: false } => {
                write!(f, "line {line}: the mirror ends with a backslash the original does not have")
            }
        }
    }
}

/// Checks invariants 3 and 4 of a mirror made from `original` read as `encoding`.
pub fn check(original: &[u8], encoding: Encoding, mirror: &[u8]) -> Result<(), Violation> {
    if std::str::from_utf8(mirror).is_err() {
        return Err(Violation::NotUtf8);
    }
    let mirror_lines = text::split_lines(mirror);
    let original_lines = match encoding {
        Encoding::Utf16le | Encoding::Utf16be | Encoding::Utf16leNobom | Encoding::Utf16beNobom => {
            let little_endian = matches!(encoding, Encoding::Utf16le | Encoding::Utf16leNobom);
            let body = match encoding {
                Encoding::Utf16le => original.strip_prefix(b"\xFF\xFE").unwrap_or(original),
                Encoding::Utf16be => original.strip_prefix(b"\xFE\xFF").unwrap_or(original),
                _ => original,
            };
            let units: Vec<u16> = judge::utf16_units(body, little_endian).collect();
            let count = text::count_lines_utf16(&units);
            if count != mirror_lines.len() {
                return Err(Violation::LineCount { original: count, mirror: mirror_lines.len() });
            }
            // The decoder maps U+005C to a backslash and nothing else to one.
            return Ok(());
        }
        Encoding::Utf8Bom | Encoding::Mixed | Encoding::Broken => {
            text::split_lines(original.strip_prefix(text::UTF8_BOM).unwrap_or(original))
        }
        _ => text::split_lines(original),
    };
    if original_lines.len() != mirror_lines.len() {
        return Err(Violation::LineCount { original: original_lines.len(), mirror: mirror_lines.len() });
    }
    for (i, ((o, o_eol), (m, m_eol))) in original_lines.iter().zip(&mirror_lines).enumerate() {
        let line = i + 1;
        if o_eol != m_eol {
            return Err(Violation::LineBreak { line });
        }
        let original_backslash = match encoding {
            Encoding::Cp932 => cp932::ends_with_backslash(o),
            Encoding::Mixed | Encoding::Broken if std::str::from_utf8(o).is_err() => cp932::ends_with_backslash(o),
            // ISO-2022-JP may shift into JIS X 0201 Roman, where 0x5C is a yen sign: the decoder
            // decides, so only the line count is checked.
            Encoding::Iso2022jp => text::ends_with_backslash(m),
            _ => text::ends_with_backslash(o),
        };
        if original_backslash != text::ends_with_backslash(m) {
            return Err(Violation::Backslash { line, original: original_backslash });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirror(bytes: &[u8], encoding: Encoding) -> String {
        let m = to_utf8(bytes, encoding).unwrap();
        check(bytes, encoding, &m).unwrap();
        String::from_utf8(m).unwrap()
    }

    #[test]
    fn cp932_comment_ending_in_a_5c_character_does_not_splice() {
        // // 表 \r\n int hidden(int);  — read as UTF-8, line 1 would swallow line 2.
        let original = b"// \x95\x5C\r\nint hidden(int);\r\n";
        assert_eq!(mirror(original, Encoding::Cp932), "// 表\r\nint hidden(int);\r\n");
    }

    #[test]
    fn a_real_backslash_stays() {
        let original = b"#define A(x) \\\r\n  (x)\r\n// \x94\x5C\r\n";
        assert_eq!(mirror(original, Encoding::Cp932), "#define A(x) \\\r\n  (x)\r\n// 能\r\n");
    }

    #[test]
    fn an_unmapped_pair_takes_its_5c_with_it() {
        assert_eq!(mirror(b"// \x85\x5C\nint x;\n", Encoding::Cp932), "// \u{FFFD}\nint x;\n");
        assert_eq!(mirror(b"// \x82\r\nint x;", Encoding::Cp932), "// \u{FFFD}\r\nint x;");
    }

    #[test]
    fn string_literal_with_a_5c_character() {
        // "表示" in a string: 95 5C 8E A6. A UTF-8 reader sees \ followed by bytes.
        assert_eq!(mirror(b"const char* s = \"\x95\x5C\x8E\xA6\";\n", Encoding::Cp932), "const char* s = \"表示\";\n");
    }

    #[test]
    fn mixed_lines_and_line_breaks() {
        let original = [&b"// \x95\x5C\r"[..], "// 表\n".as_bytes(), b"int a;\r\n", b"end"].concat();
        assert_eq!(mirror(&original, Encoding::Mixed), "// 表\r// 表\nint a;\r\nend");
    }

    #[test]
    fn utf16_and_bom() {
        let le: Vec<u8> =
            [0xFF, 0xFE].into_iter().chain("// 表\r\nint a;\r\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(mirror(&le, Encoding::Utf16le), "// 表\r\nint a;\r\n");
        assert_eq!(mirror(&[text::UTF8_BOM, b"int a;\n"].concat(), Encoding::Utf8Bom), "int a;\n");
    }

    #[test]
    fn the_check_catches_a_decoder_that_splices() {
        // What Python's cp932 with errors="replace" gives for 85 5C: U+FFFD and a backslash.
        let wrong = "// \u{FFFD}\\\nint x;\n".as_bytes();
        assert_eq!(
            check(b"// \x85\x5C\nint x;\n", Encoding::Cp932, wrong),
            Err(Violation::Backslash { line: 1, original: false })
        );
        assert_eq!(check(b"a\nb\n", Encoding::Ascii, b"a\n"), Err(Violation::LineCount { original: 2, mirror: 1 }));
        assert_eq!(check(b"a\r\n", Encoding::Ascii, b"a\n"), Err(Violation::LineBreak { line: 1 }));
        assert_eq!(check(b"a\n", Encoding::Ascii, b"\xFF\n"), Err(Violation::NotUtf8));
    }

    #[test]
    fn ambiguous_and_binary_are_not_read() {
        assert_eq!(to_utf8(b"x", Encoding::Binary), None);
        assert_eq!(to_utf8(b"x", Encoding::AmbiguousCp932EucJp), None);
        assert_eq!(to_utf8(b"\xC3\xBD", Encoding::AmbiguousUtf8Cp932).as_deref(), Some("ý".as_bytes()));
    }
}
