//! The columns of a file as a misreading client sees it and as clangd sees it.
//!
//! A client that reads a CP932 original as UTF-8 (an agent's LSP client does) has its bytes decoded
//! as UTF-8 with each invalid run as one U+FFFD; clangd reads the mirror. The lines are the same;
//! the columns are not: `// 表` is five columns to the client (`/`, `/`, ` `, U+FFFD for `95`,
//! `\` for `5C`) and four in the mirror. Both readings cut the line's bytes into units, so a
//! column goes through the byte where its unit starts.

use crate::judge::Encoding;
use crate::{cp932, text};

/// How LSP counts columns (the negotiated `positionEncoding`; UTF-16 unless both sides agree on
/// another).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Units {
    Utf8,
    #[default]
    Utf16,
    Utf32,
}

impl Units {
    pub fn from_name(name: &str) -> Units {
        match name {
            "utf-8" => Units::Utf8,
            "utf-32" => Units::Utf32,
            _ => Units::Utf16,
        }
    }

    fn width(self, c: char) -> u32 {
        match self {
            Units::Utf8 => c.len_utf8() as u32,
            Units::Utf16 => c.len_utf16() as u32,
            Units::Utf32 => 1,
        }
    }
}

/// Which way a column goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    /// From the client's reading to the mirror's (a request).
    ToMirror,
    /// From the mirror's reading to the client's (an answer).
    ToClient,
}

/// One line: where each unit starts, as (byte, column), in both readings, each ending with
/// (the line's length in bytes, its width).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    client: Vec<(u32, u32)>,
    mirror: Vec<(u32, u32)>,
}

/// The column maps of a file; lines whose readings agree have none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileMap {
    lines: Vec<Option<Line>>,
}

impl FileMap {
    /// The map of an original read as `encoding`, or `None` when the client's reading is the
    /// mirror's (UTF-8), or when the lines of the two do not match (UTF-16, ISO-2022-JP: not
    /// translated).
    pub fn new(bytes: &[u8], encoding: Encoding, units: Units) -> Option<FileMap> {
        let reader = match encoding {
            Encoding::Cp932 => Reader::Cp932,
            Encoding::Mixed | Encoding::Broken => Reader::Utf8ElseCp932,
            Encoding::EucJp => Reader::EucJp,
            _ => return None,
        };
        let has_bom = bytes.starts_with(text::UTF8_BOM) && reader == Reader::Utf8ElseCp932;
        let mut lines = Vec::new();
        for (i, (line, _)) in text::split_lines(bytes).into_iter().enumerate() {
            // The client keeps a BOM as U+FEFF on the first line; the mirror drops it.
            let skip = if i == 0 && has_bom { text::UTF8_BOM.len() } else { 0 };
            let client = client_units(line, units);
            let mirror = mirror_units(line, skip, reader, units);
            lines.push((client != mirror).then_some(Line { client, mirror }));
        }
        Some(FileMap { lines })
    }

    /// Whether any line's columns differ.
    pub fn differs(&self) -> bool {
        self.lines.iter().any(Option::is_some)
    }

    /// A column of `line` moved the given way. `end` rounds up (the end of a range), else down.
    pub fn column(&self, line: u32, column: u32, way: Way, end: bool) -> u32 {
        let Some(Some(l)) = self.lines.get(line as usize) else { return column };
        let (from, to) = match way {
            Way::ToMirror => (&l.client, &l.mirror),
            Way::ToClient => (&l.mirror, &l.client),
        };
        let byte = column_to_byte(from, column, end);
        match byte {
            Ok(byte) => byte_to_column(to, byte, end),
            // Past the end of the line: keep the distance from its end.
            Err(beyond) => to.last().map_or(column, |&(_, width)| width + beyond),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reader {
    Cp932,
    Utf8ElseCp932,
    EucJp,
}

/// The units of the line as `String::from_utf8_lossy` reads it (one U+FFFD per invalid run, as
/// WHATWG's decoder and Node's do).
fn client_units(line: &[u8], units: Units) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let (mut byte, mut column) = (0u32, 0u32);
    for chunk in line.utf8_chunks() {
        for c in chunk.valid().chars() {
            out.push((byte, column));
            byte += c.len_utf8() as u32;
            column += units.width(c);
        }
        if !chunk.invalid().is_empty() {
            out.push((byte, column));
            byte += chunk.invalid().len() as u32;
            column += units.width(cp932::REPLACEMENT);
        }
    }
    out.push((byte, column));
    out
}

/// The units of the line as the mirror reads it, from byte `skip` on.
fn mirror_units(line: &[u8], skip: usize, reader: Reader, units: Units) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut column = 0u32;
    let body = &line[skip.min(line.len())..];
    let mut byte = skip as u32;
    let mut push = |len: usize, c: char, out: &mut Vec<(u32, u32)>| {
        out.push((byte, column));
        byte += len as u32;
        column += units.width(c);
    };
    match (reader, std::str::from_utf8(body)) {
        (Reader::Utf8ElseCp932, Ok(s)) => {
            for c in s.chars() {
                push(c.len_utf8(), c, &mut out);
            }
        }
        (Reader::Cp932 | Reader::Utf8ElseCp932, _) => {
            for unit in cp932::units(body) {
                let (len, c) = match unit {
                    cp932::Unit::Single(b) if b < 0x80 => (1, char::from(b)),
                    // Half-width katakana: one BMP character.
                    cp932::Unit::Single(_) => (1, '\u{FF61}'),
                    cp932::Unit::Pair(lead, trail) => (2, cp932::pair(lead, trail).unwrap_or(cp932::REPLACEMENT)),
                    cp932::Unit::Invalid(_) => (1, cp932::REPLACEMENT),
                };
                push(len, c, &mut out);
            }
        }
        (Reader::EucJp, _) => {
            let mut i = 0;
            while i < body.len() {
                let len = match body[i] {
                    0x8F => 3,
                    0x8E | 0xA1..=0xFE => 2,
                    _ => 1,
                }
                .min(body.len() - i);
                let c = if len == 1 && body[i] < 0x80 { char::from(body[i]) } else { '\u{3000}' };
                push(len, c, &mut out);
                i += len;
            }
        }
    }
    out.push((byte, column));
    out
}

/// The byte a column of these units stands at; `Err(n)` when it is `n` past the line's end.
fn column_to_byte(units: &[(u32, u32)], column: u32, end: bool) -> Result<u32, u32> {
    let &(last_byte, width) = units.last().unwrap_or(&(0, 0));
    if column >= width {
        return if column == width { Ok(last_byte) } else { Err(column - width) };
    }
    // The unit that holds the column.
    let i = units.partition_point(|&(_, c)| c <= column) - 1;
    let (byte, start) = units[i];
    if column == start || !end { Ok(byte) } else { Ok(units[i + 1].0) }
}

/// The column of these units at a byte: the start of the unit holding it, or the next when `end`.
fn byte_to_column(units: &[(u32, u32)], byte: u32, end: bool) -> u32 {
    if units.first().is_none_or(|&(b, _)| byte <= b) {
        return 0;
    }
    let i = units.partition_point(|&(b, _)| b <= byte) - 1;
    let (start_byte, column) = units[i];
    if byte == start_byte || !end || i + 1 >= units.len() { column } else { units[i + 1].1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(bytes: &[u8]) -> FileMap {
        FileMap::new(bytes, Encoding::Cp932, Units::Utf16).unwrap()
    }

    #[test]
    fn a_5c_character_is_two_client_columns_and_one_mirror_column() {
        // `x = "表"; hidden` — the client sees U+FFFD and `\` for 表.
        let line = b"x = \"\x95\x5C\"; hidden(1);";
        let m = map(line);
        let client = String::from_utf8_lossy(line);
        let client_col = client.encode_utf16().count() as u32 - "hidden(1);".len() as u32;
        let mirror_col = "x = \"表\"; ".encode_utf16().count() as u32;
        assert_eq!(m.column(0, client_col, Way::ToMirror, false), mirror_col);
        assert_eq!(m.column(0, mirror_col, Way::ToClient, false), client_col);
        // Inside 表: the client's column of `\` goes to the start of 表 (or its end, for an end).
        let hyo = 5;
        assert_eq!(m.column(0, hyo + 1, Way::ToMirror, false), hyo);
        assert_eq!(m.column(0, hyo + 1, Way::ToMirror, true), hyo + 1);
        assert_eq!(m.column(0, hyo, Way::ToClient, false), hyo);
        assert_eq!(m.column(0, hyo + 1, Way::ToClient, false), hyo + 2);
    }

    #[test]
    fn ascii_lines_have_no_map_and_past_the_end_keeps_the_distance() {
        let m = map(b"int a;\r\n// \x95\x5C\r\n");
        assert!(m.differs());
        assert_eq!(m.column(0, 3, Way::ToMirror, false), 3);
        assert_eq!(m.column(1, 5, Way::ToMirror, false), 4);
        assert_eq!(m.column(1, 7, Way::ToMirror, false), 6);
        assert_eq!(m.column(9, 7, Way::ToMirror, false), 7);
    }

    #[test]
    fn invalid_runs_and_utf8_counting() {
        // 82 at the end: one U+FFFD for both; 85 5C: the client sees U+FFFD then `\`.
        let m = map(b"a\x85\x5Cb\x82");
        assert_eq!(m.column(0, 3, Way::ToMirror, false), 2, "b");
        assert_eq!(m.column(0, 2, Way::ToClient, false), 3);
        let m8 = FileMap::new("// 表".as_bytes(), Encoding::Cp932, Units::Utf8);
        assert!(m8.is_some());
        assert!(FileMap::new(b"x", Encoding::Utf8, Units::Utf16).is_none());
    }

    #[test]
    fn mixed_files_map_only_their_cp932_lines() {
        let bytes = [&b"// \x95\x5C\r\n"[..], "// 表\r\n".as_bytes()].concat();
        let m = FileMap::new(&bytes, Encoding::Mixed, Units::Utf16).unwrap();
        assert_eq!(m.column(0, 5, Way::ToMirror, false), 4);
        assert_eq!(m.column(1, 4, Way::ToMirror, false), 4);
    }
}
