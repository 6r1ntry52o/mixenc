//! A text file as lines: each line's bytes as they are on disk, its text, and the encoding its
//! bytes are in. The gate (the edit tools and the commit gate) works line by line, so that a line
//! whose text did not change keeps its bytes, line break included.

use std::ops::Range;

use crate::judge::{self, Encoding};
use crate::{cp932, text};

/// A line break.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Eol {
    None,
    Lf,
    CrLf,
    Cr,
}

impl Eol {
    fn from_ascii(bytes: &[u8]) -> Eol {
        match bytes {
            b"\n" => Eol::Lf,
            b"\r\n" => Eol::CrLf,
            b"\r" => Eol::Cr,
            _ => Eol::None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Eol::None => "",
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
            Eol::Cr => "\r",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Eol::None => "none",
            Eol::Lf => "LF",
            Eol::CrLf => "CRLF",
            Eol::Cr => "CR",
        }
    }
}

/// The encoding of one line's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnc {
    Utf8,
    Cp932,
    EucJp,
    Iso2022jp,
    Utf16le,
    Utf16be,
}

impl LineEnc {
    pub fn name(self) -> &'static str {
        match self {
            LineEnc::Utf8 => "UTF-8",
            LineEnc::Cp932 => "CP932",
            LineEnc::EucJp => "EUC-JP",
            LineEnc::Iso2022jp => "ISO-2022-JP",
            LineEnc::Utf16le => "UTF-16LE",
            LineEnc::Utf16be => "UTF-16BE",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The line's bytes on disk, its line break included.
    pub raw: Vec<u8>,
    /// Its text, without the line break; U+FFFD where the bytes read in no character.
    pub text: String,
    pub eol: Eol,
    pub enc: LineEnc,
    /// Every byte read as a character.
    pub ok: bool,
}

impl Line {
    /// The bytes of the text, without the line break.
    pub fn content(&self) -> &[u8] {
        let eol = match self.enc {
            LineEnc::Utf16le | LineEnc::Utf16be => self.eol.as_str().len() * 2,
            _ => self.eol.as_str().len(),
        };
        &self.raw[..self.raw.len().saturating_sub(eol)]
    }

    /// Each character of the text with its bytes in [`Line::content`], for the encodings whose
    /// characters can be told apart without state (`None` for the others).
    pub fn char_bytes(&self) -> Option<Vec<(char, Range<usize>)>> {
        let content = self.content();
        match self.enc {
            LineEnc::Utf8 if self.ok => Some(self.text.char_indices().map(|(i, c)| (c, i..i + c.len_utf8())).collect()),
            LineEnc::Cp932 => {
                let mut at = 0;
                Some(
                    cp932::units(content)
                        .map(|unit| {
                            let len = cp932::unit_len(unit);
                            let item = (cp932::unit_char(unit).unwrap_or(cp932::REPLACEMENT), at..at + len);
                            at += len;
                            item
                        })
                        .collect(),
                )
            }
            _ => None,
        }
    }
}

/// A file read as lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    /// The byte order mark, if the file has one.
    pub bom: Vec<u8>,
    pub lines: Vec<Line>,
}

#[derive(Clone, Copy)]
enum Read {
    Utf8,
    Cp932,
    /// UTF-8 where the line reads so, else CP932 (as the mirror reads a mixed file).
    Utf8ElseCp932,
    EucJp,
    Iso2022jp,
    Utf16(bool),
}

/// The lines of `bytes` read as `read_as`, or `None` for what is not read until a person decides
/// (ambiguous between CP932 and EUC-JP, binary).
pub fn parse(bytes: &[u8], read_as: Encoding) -> Option<Doc> {
    let bom_of = |bom: &[u8]| if bytes.starts_with(bom) { bom.len() } else { 0 };
    let (bom, read) = match read_as {
        Encoding::Ascii | Encoding::Utf8 | Encoding::AmbiguousUtf8Cp932 => (0, Read::Utf8),
        Encoding::Utf8Bom => (bom_of(text::UTF8_BOM), Read::Utf8),
        Encoding::Cp932 => (0, Read::Cp932),
        Encoding::Mixed | Encoding::Broken => (bom_of(text::UTF8_BOM), Read::Utf8ElseCp932),
        Encoding::EucJp => (0, Read::EucJp),
        Encoding::Iso2022jp => (0, Read::Iso2022jp),
        Encoding::Utf16le => (bom_of(b"\xFF\xFE"), Read::Utf16(true)),
        Encoding::Utf16be => (bom_of(b"\xFE\xFF"), Read::Utf16(false)),
        Encoding::Utf16leNobom => (0, Read::Utf16(true)),
        Encoding::Utf16beNobom => (0, Read::Utf16(false)),
        Encoding::AmbiguousCp932EucJp | Encoding::Binary => return None,
    };
    let body = &bytes[bom..];
    let lines = match read {
        Read::Utf16(little_endian) => utf16_lines(body, little_endian),
        _ => text::split_lines(body).into_iter().map(|(content, eol)| byte_line(content, eol, read)).collect(),
    };
    Some(Doc { bom: bytes[..bom].to_vec(), lines })
}

fn byte_line(content: &[u8], eol: &[u8], read: Read) -> Line {
    let cp932 = || {
        let mut s = String::new();
        let ok = cp932::decode_line(content, &mut s);
        (s, LineEnc::Cp932, ok)
    };
    let strict = |encoding: &'static encoding_rs::Encoding, enc: LineEnc| match encoding
        .decode_without_bom_handling_and_without_replacement(content)
    {
        Some(s) => (s.into_owned(), enc, true),
        None => (encoding.decode_without_bom_handling(content).0.into_owned(), enc, false),
    };
    let (text, enc, ok) = match read {
        Read::Utf8 => match std::str::from_utf8(content) {
            Ok(s) => (s.to_string(), LineEnc::Utf8, true),
            Err(_) => (String::from_utf8_lossy(content).into_owned(), LineEnc::Utf8, false),
        },
        Read::Cp932 => cp932(),
        Read::Utf8ElseCp932 => match std::str::from_utf8(content) {
            Ok(s) => (s.to_string(), LineEnc::Utf8, true),
            Err(_) => cp932(),
        },
        Read::EucJp => strict(encoding_rs::EUC_JP, LineEnc::EucJp),
        Read::Iso2022jp => strict(encoding_rs::ISO_2022_JP, LineEnc::Iso2022jp),
        Read::Utf16(_) => unreachable!("UTF-16 is split by units"),
    };
    Line { raw: [content, eol].concat(), text, eol: Eol::from_ascii(eol), enc, ok }
}

fn utf16_lines(body: &[u8], little_endian: bool) -> Vec<Line> {
    let enc = if little_endian { LineEnc::Utf16le } else { LineEnc::Utf16be };
    let units: Vec<u16> = judge::utf16_units(body, little_endian).collect();
    let line = |start: usize, content_end: usize, end: usize| {
        let eol = match &units[content_end..end] {
            [0x0A] => Eol::Lf,
            [0x0D, 0x0A] => Eol::CrLf,
            [0x0D] => Eol::Cr,
            _ => Eol::None,
        };
        let decoded: Result<String, _> = char::decode_utf16(units[start..content_end].iter().copied()).collect();
        let (text, ok) = match decoded {
            Ok(text) => (text, true),
            Err(_) => (
                char::decode_utf16(units[start..content_end].iter().copied())
                    .map(|c| c.unwrap_or(cp932::REPLACEMENT))
                    .collect(),
                false,
            ),
        };
        Line { raw: body[start * 2..end * 2].to_vec(), text, eol, enc, ok }
    };
    let mut lines = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < units.len() {
        let end = match units[i] {
            0x0A => i + 1,
            0x0D if units.get(i + 1) == Some(&0x0A) => i + 2,
            0x0D => i + 1,
            _ => {
                i += 1;
                continue;
            }
        };
        lines.push(line(start, i, end));
        i = end;
        start = end;
    }
    if start < units.len() {
        lines.push(line(start, units.len(), units.len()));
    }
    if body.len() % 2 == 1 {
        // A byte left over, which no UTF-16 unit takes.
        let last = body[body.len() - 1];
        match lines.last_mut() {
            Some(l) if l.eol == Eol::None => {
                l.raw.push(last);
                l.text.push(cp932::REPLACEMENT);
                l.ok = false;
            }
            _ => lines.push(Line { raw: vec![last], text: cp932::REPLACEMENT.into(), eol: Eol::None, enc, ok: false }),
        }
    }
    lines
}

impl Doc {
    /// The text with every line break as `\n` (what the edit tools search).
    pub fn normalized(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(&line.text);
            if line.eol != Eol::None {
                out.push('\n');
            }
        }
        out
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.bom.clone();
        for line in &self.lines {
            out.extend_from_slice(&line.raw);
        }
        out
    }

    /// The line break most lines end with (CRLF on a tie, or when no line has one).
    pub fn eol_style(&self) -> Eol {
        let count = |eol: Eol| self.lines.iter().filter(|l| l.eol == eol).count();
        let (crlf, lf, cr) = (count(Eol::CrLf), count(Eol::Lf), count(Eol::Cr));
        if lf > crlf && lf >= cr {
            Eol::Lf
        } else if cr > crlf && cr > lf {
            Eol::Cr
        } else {
            Eol::CrLf
        }
    }

    /// The encoding most non-ASCII lines are in, counted as the judgement counts them (CP932 on a
    /// tie): what a mixed file's new lines are written in.
    pub fn major(&self) -> LineEnc {
        let body: Vec<u8> = self.lines.iter().flat_map(|l| l.raw.iter().copied()).collect();
        let counts = judge::line_counts(&body);
        if counts.utf8 > counts.cp932 { LineEnc::Utf8 } else { LineEnc::Cp932 }
    }
}

/// The bytes of a line of `text` ending with `eol`, in `enc`; or the characters `enc` cannot
/// write (empty when no single character is to blame).
pub fn encode_line(text: &str, eol: Eol, enc: LineEnc) -> Result<Vec<u8>, Vec<char>> {
    let utf16 = |little_endian: bool| -> Vec<u8> {
        text.encode_utf16()
            .chain(eol.as_str().encode_utf16())
            .flat_map(|u| if little_endian { u.to_le_bytes() } else { u.to_be_bytes() })
            .collect()
    };
    let mut out = match enc {
        LineEnc::Utf8 => text.as_bytes().to_vec(),
        LineEnc::Utf16le => return Ok(utf16(true)),
        LineEnc::Utf16be => return Ok(utf16(false)),
        LineEnc::Cp932 | LineEnc::EucJp | LineEnc::Iso2022jp => match encode_text(text, enc) {
            Some(bytes) => bytes,
            None => return Err(unwritable(text, enc)),
        },
    };
    out.extend_from_slice(eol.as_str().as_bytes());
    Ok(out)
}

/// `text` in a byte encoding, when it reads back as the same text: Windows' CP932 without
/// best-fit substitutes (`¥` does not become a backslash), encoding_rs for the others.
fn encode_text(text: &str, enc: LineEnc) -> Option<Vec<u8>> {
    let bytes = match enc {
        LineEnc::Cp932 => crate::win::cp932_encode(text)?,
        LineEnc::EucJp | LineEnc::Iso2022jp => {
            let encoding = if enc == LineEnc::EucJp { encoding_rs::EUC_JP } else { encoding_rs::ISO_2022_JP };
            let (bytes, _, unmappable) = encoding.encode(text);
            if unmappable {
                return None;
            }
            bytes.into_owned()
        }
        LineEnc::Utf8 | LineEnc::Utf16le | LineEnc::Utf16be => return None,
    };
    (decode_text(&bytes, enc)? == text).then_some(bytes)
}

fn decode_text(bytes: &[u8], enc: LineEnc) -> Option<String> {
    match enc {
        LineEnc::Cp932 => {
            let mut s = String::new();
            cp932::decode_line(bytes, &mut s).then_some(s)
        }
        LineEnc::EucJp => {
            encoding_rs::EUC_JP.decode_without_bom_handling_and_without_replacement(bytes).map(Into::into)
        }
        LineEnc::Iso2022jp => {
            encoding_rs::ISO_2022_JP.decode_without_bom_handling_and_without_replacement(bytes).map(Into::into)
        }
        LineEnc::Utf8 | LineEnc::Utf16le | LineEnc::Utf16be => None,
    }
}

fn unwritable(text: &str, enc: LineEnc) -> Vec<char> {
    let mut chars = Vec::new();
    for c in text.chars() {
        if !chars.contains(&c) && encode_text(c.encode_utf8(&mut [0; 4]), enc).is_none() {
            chars.push(c);
        }
    }
    chars
}

/// The character CP932 has in the place of `c`, for the characters JIS and CP932 map apart (JIS's
/// wave dash `〜` is the fullwidth tilde `～` in CP932).
pub fn cp932_nearest(c: char) -> Option<char> {
    const PAIRS: [(char, char); 7] = [
        ('\u{301C}', '\u{FF5E}'),
        ('\u{2016}', '\u{2225}'),
        ('\u{2212}', '\u{FF0D}'),
        ('\u{00A2}', '\u{FFE0}'),
        ('\u{00A3}', '\u{FFE1}'),
        ('\u{00AC}', '\u{FFE2}'),
        ('\u{2014}', '\u{2015}'),
    ];
    PAIRS.iter().find(|(jis, _)| *jis == c).map(|(_, cp932)| *cp932)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_keep_their_bytes() {
        let bytes = b"// \x95\x5C\r\nint a;\n\xFA\x4A";
        let doc = parse(bytes, Encoding::Cp932).unwrap();
        assert_eq!(doc.lines.len(), 3);
        assert_eq!(doc.lines[0].text, "// 表");
        assert_eq!(doc.lines[0].eol, Eol::CrLf);
        assert_eq!(doc.lines[2].text, "Ⅰ");
        assert_eq!(doc.to_bytes(), bytes);
        assert_eq!(doc.normalized(), "// 表\nint a;\nⅠ");
        assert_eq!(doc.lines[2].char_bytes().unwrap(), [('Ⅰ', 0..2)]);
    }

    #[test]
    fn utf16_lines_and_bom() {
        let bytes: Vec<u8> =
            [0xFF, 0xFE].into_iter().chain("a\r\n表\nb".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let doc = parse(&bytes, Encoding::Utf16le).unwrap();
        assert_eq!(doc.bom, [0xFF, 0xFE]);
        let texts: Vec<&str> = doc.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["a", "表", "b"]);
        assert_eq!(doc.lines[0].content(), b"a\0");
        assert_eq!(doc.to_bytes(), bytes);
        assert_eq!(encode_line("表", Eol::Lf, LineEnc::Utf16le).unwrap(), doc.lines[1].raw);
    }

    #[test]
    fn mixed_lines_keep_their_encodings() {
        let bytes = [&b"// \x95\x5C\n"[..], "// 表\n".as_bytes(), b"// \x95\x5C\n"].concat();
        let doc = parse(&bytes, Encoding::Mixed).unwrap();
        let encs: Vec<LineEnc> = doc.lines.iter().map(|l| l.enc).collect();
        assert_eq!(encs, [LineEnc::Cp932, LineEnc::Utf8, LineEnc::Cp932]);
        assert_eq!(doc.major(), LineEnc::Cp932);
        assert_eq!(doc.eol_style(), Eol::Lf);
    }

    #[test]
    fn writes_cp932_as_visual_studio_does() {
        // Ⅰ has an NEC form (87 54) and an IBM form (FA 4A); Windows writes NEC's.
        assert_eq!(encode_line("Ⅰ表", Eol::CrLf, LineEnc::Cp932).unwrap(), b"\x87\x54\x95\x5C\r\n");
        assert_eq!(encode_line("a😀b𠮷", Eol::None, LineEnc::Cp932), Err(vec!['😀', '𠮷']));
        // No best fit: ¥ would become a backslash, a line splice at the end of a comment.
        assert_eq!(encode_line("¥", Eol::None, LineEnc::Cp932), Err(vec!['¥']));
        assert_eq!(cp932_nearest('〜'), Some('～'));
        for jis in "〜‖−¢£¬—".chars() {
            let near = cp932_nearest(jis).unwrap();
            assert!(encode_line(&near.to_string(), Eol::None, LineEnc::Cp932).is_ok(), "{near}");
            assert!(encode_line(&jis.to_string(), Eol::None, LineEnc::Cp932).is_err(), "{jis}");
        }
        assert_eq!(cp932_nearest('¥'), None);
        assert_eq!(encode_line("ｱ", Eol::Lf, LineEnc::EucJp).unwrap(), b"\x8E\xB1\n");
        assert_eq!(encode_line("¥", Eol::None, LineEnc::EucJp), Err(vec!['¥']));
    }
}
