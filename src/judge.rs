//! The judgement: which encoding a file is in. Every file gets exactly one. When two readings are
//! both possible, the judgement says so (`ambiguous-*`) instead of guessing, and the file is not
//! mirrored until a person decides in the override table.
//!
//! The order:
//! 1. a BOM (UTF-8, UTF-16LE, UTF-16BE);
//! 2. a NUL byte: UTF-16 without a BOM if it reads strictly and the NULs fall on one side, else binary;
//! 3. ISO-2022-JP escape sequences in 7-bit text;
//! 4. line by line: strict UTF-8, CP932 (two bytes as one unit), both or neither;
//! 5. all ASCII, UTF-8, CP932, or UTF-8 and CP932 lines mixed;
//! 6. a line that reads neither way: EUC-JP if the whole file reads strictly so, else broken;
//! 7. two readings that both hold: ambiguous.
//!
//! UTF-8 text often reads as valid CP932 too ("表示する" or "→" in UTF-8 are valid CP932 units),
//! while CP932 text almost never reads as valid UTF-8 with a character of three bytes or more (that
//! needs a JIS level-2 kanji followed by half-width katakana, and kana break UTF-8 at once). So a
//! line that reads both ways counts as UTF-8 when its UTF-8 reading has a character from U+0800 up,
//! and only a file whose every non-ASCII line reads both ways without one (only "©", "°", "é" and
//! the like, which CP932 reads as half-width katakana) is ambiguous between UTF-8 and CP932. Such a
//! file is mirrored as UTF-8 and listed: a file that is valid UTF-8 cannot hold the CP932 trap (a
//! lead byte before `0x5C` breaks UTF-8), so its UTF-8 reading keeps the lines whatever it really is.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{cp932, text};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Encoding {
    Ascii,
    Utf8,
    Utf8Bom,
    Utf16le,
    Utf16be,
    Utf16leNobom,
    Utf16beNobom,
    Cp932,
    EucJp,
    #[serde(rename = "iso-2022-jp")]
    Iso2022jp,
    /// UTF-8 lines and CP932 lines in one file.
    Mixed,
    /// A line that reads in no encoding; the readable lines are mirrored, the rest as U+FFFD.
    Broken,
    /// Every non-ASCII line reads both as UTF-8 and as CP932, with no character from U+0800 up in
    /// its UTF-8 reading. Mirrored as UTF-8.
    AmbiguousUtf8Cp932,
    /// The file reads both as CP932 and as EUC-JP, and has no CP932 kana.
    AmbiguousCp932EucJp,
    /// NUL bytes that are not UTF-16.
    Binary,
}

impl Encoding {
    pub const ALL: [Encoding; 15] = [
        Encoding::Ascii,
        Encoding::Utf8,
        Encoding::Utf8Bom,
        Encoding::Utf16le,
        Encoding::Utf16be,
        Encoding::Utf16leNobom,
        Encoding::Utf16beNobom,
        Encoding::Cp932,
        Encoding::EucJp,
        Encoding::Iso2022jp,
        Encoding::Mixed,
        Encoding::Broken,
        Encoding::AmbiguousUtf8Cp932,
        Encoding::AmbiguousCp932EucJp,
        Encoding::Binary,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Encoding::Ascii => "ascii",
            Encoding::Utf8 => "utf8",
            Encoding::Utf8Bom => "utf8-bom",
            Encoding::Utf16le => "utf16le",
            Encoding::Utf16be => "utf16be",
            Encoding::Utf16leNobom => "utf16le-nobom",
            Encoding::Utf16beNobom => "utf16be-nobom",
            Encoding::Cp932 => "cp932",
            Encoding::EucJp => "euc-jp",
            Encoding::Iso2022jp => "iso-2022-jp",
            Encoding::Mixed => "mixed",
            Encoding::Broken => "broken",
            Encoding::AmbiguousUtf8Cp932 => "ambiguous-utf8-cp932",
            Encoding::AmbiguousCp932EucJp => "ambiguous-cp932-euc-jp",
            Encoding::Binary => "binary",
        }
    }

    /// Whether the file is mirrored on its judgement alone (the others wait for the override table).
    pub fn is_decided(self) -> bool {
        !matches!(self, Encoding::AmbiguousCp932EucJp | Encoding::Binary)
    }

    /// Whether the doctor lists such files to know about: they are mirrored, but something about
    /// them is unusual (an editor reads a mixed file wrongly, MSVC cannot read EUC-JP, ...).
    pub fn is_notice(self) -> bool {
        matches!(
            self,
            Encoding::Utf16leNobom
                | Encoding::Utf16beNobom
                | Encoding::EucJp
                | Encoding::Iso2022jp
                | Encoding::Mixed
                | Encoding::Broken
                | Encoding::AmbiguousUtf8Cp932
        )
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Encoding {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Encoding::ALL.into_iter().find(|e| e.name() == s).ok_or_else(|| format!("unknown encoding `{s}`"))
    }
}

pub fn judge(bytes: &[u8]) -> Encoding {
    if let Some(rest) = bytes.strip_prefix(text::UTF8_BOM) {
        return if std::str::from_utf8(rest).is_ok() {
            Encoding::Utf8Bom
        } else if by_lines(rest) == Encoding::Mixed {
            Encoding::Mixed
        } else {
            Encoding::Broken
        };
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return if utf16(rest, true).is_some() { Encoding::Utf16le } else { Encoding::Binary };
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        return if utf16(rest, false).is_some() { Encoding::Utf16be } else { Encoding::Binary };
    }
    if bytes.contains(&0) {
        return utf16_without_bom(bytes).unwrap_or(Encoding::Binary);
    }
    if bytes.is_ascii() && has_iso2022jp_escape(bytes) {
        return if encoding_rs::ISO_2022_JP.decode_without_bom_handling_and_without_replacement(bytes).is_some() {
            Encoding::Iso2022jp
        } else {
            Encoding::Broken
        };
    }
    by_lines(bytes)
}

/// Strictly decoded UTF-16 (no unpaired surrogates, an even length).
pub fn utf16(bytes: &[u8], little_endian: bool) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    char::decode_utf16(utf16_units(bytes, little_endian)).collect::<Result<String, _>>().ok()
}

pub fn utf16_units(bytes: &[u8], little_endian: bool) -> impl Iterator<Item = u16> + '_ {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(move |c| if little_endian { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
}

fn utf16_without_bom(bytes: &[u8]) -> Option<Encoding> {
    let (mut even, mut odd) = (0usize, 0usize);
    for (i, &b) in bytes.iter().enumerate() {
        if b == 0 {
            if i % 2 == 0 { even += 1 } else { odd += 1 }
        }
    }
    // ASCII in UTF-16LE puts its zero byte at odd offsets, in UTF-16BE at even ones. Other
    // characters with a zero byte (U+3000, U+4E00, ...) fall on the other side, so the NULs need
    // only lean clearly to one side.
    let little_endian = odd >= even;
    let (major, minor) = if little_endian { (odd, even) } else { (even, odd) };
    if minor * 9 > major {
        return None;
    }
    let text = utf16(bytes, little_endian)?;
    let controls = text.chars().any(|c| c < ' ' && !matches!(c, '\t' | '\n' | '\r' | '\x0B' | '\x0C'));
    if controls {
        return None;
    }
    Some(if little_endian { Encoding::Utf16leNobom } else { Encoding::Utf16beNobom })
}

fn has_iso2022jp_escape(bytes: &[u8]) -> bool {
    [&b"\x1B$@"[..], b"\x1B$B", b"\x1B(J", b"\x1B(I"].iter().any(|seq| bytes.windows(seq.len()).any(|w| w == *seq))
}

fn by_lines(bytes: &[u8]) -> Encoding {
    if bytes.is_ascii() {
        return Encoding::Ascii;
    }
    let (mut utf8, mut cp932, mut neither) = (0usize, 0usize, 0usize);
    let mut low_lead = false;
    for (line, _) in text::split_lines(bytes) {
        if line.is_ascii() {
            continue;
        }
        let check = cp932::check_line(line);
        low_lead |= check.ok && check.low_lead;
        match (std::str::from_utf8(line), check.ok) {
            (Ok(_), false) => utf8 += 1,
            (Ok(s), true) if s.chars().any(|c| c >= '\u{800}') => utf8 += 1,
            (Ok(_), true) => {}
            (Err(_), true) => cp932 += 1,
            (Err(_), false) => neither += 1,
        }
    }
    if neither > 0 {
        return if is_euc_jp(bytes) { Encoding::EucJp } else { Encoding::Broken };
    }
    match (utf8 > 0, cp932 > 0) {
        (true, true) => Encoding::Mixed,
        (true, false) => Encoding::Utf8,
        (false, true) if !low_lead && is_euc_jp(bytes) => Encoding::AmbiguousCp932EucJp,
        (false, true) => Encoding::Cp932,
        (false, false) => Encoding::AmbiguousUtf8Cp932,
    }
}

fn is_euc_jp(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b < 0x80 || b == 0x8E || b == 0x8F || (0xA1..=0xFE).contains(&b))
        && encoding_rs::EUC_JP.decode_without_bom_handling_and_without_replacement(bytes).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sjis(s: &str) -> Vec<u8> {
        let (bytes, _, unmappable) = encoding_rs::SHIFT_JIS.encode(s);
        assert!(!unmappable);
        bytes.into_owned()
    }

    fn euc(s: &str) -> Vec<u8> {
        encoding_rs::EUC_JP.encode(s).0.into_owned()
    }

    #[test]
    fn plain_encodings() {
        assert_eq!(judge(b"int main() {}\r\n"), Encoding::Ascii);
        assert_eq!(judge(b""), Encoding::Ascii);
        assert_eq!(judge("// 表示する\nint a;\n".as_bytes()), Encoding::Utf8);
        assert_eq!(judge(&sjis("// 表示する\nint a;\n")), Encoding::Cp932);
        assert_eq!(judge(&[text::UTF8_BOM, "// 表\n".as_bytes()].concat()), Encoding::Utf8Bom);
    }

    #[test]
    fn utf16_with_and_without_bom() {
        let le: Vec<u8> = "// 表示　です\r\nint a;\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let be: Vec<u8> = "// 表示\nint a;\n".encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(judge(&[&b"\xFF\xFE"[..], &le].concat()), Encoding::Utf16le);
        assert_eq!(judge(&[&b"\xFE\xFF"[..], &be].concat()), Encoding::Utf16be);
        assert_eq!(judge(&le), Encoding::Utf16leNobom);
        assert_eq!(judge(&be), Encoding::Utf16beNobom);
        assert_eq!(judge(b"\x00\x01\x02\x03binary\x00\x00\xFF"), Encoding::Binary);
    }

    #[test]
    fn iso2022jp_and_euc_jp() {
        let jis = encoding_rs::ISO_2022_JP.encode("// 表示\nint a;\n").0.into_owned();
        assert_eq!(judge(&jis), Encoding::Iso2022jp);
        // ア in EUC-JP is A5 A2, which CP932 reads as two half-width katakana: both readings hold.
        assert_eq!(judge(&euc("// アイウ\n")), Encoding::AmbiguousCp932EucJp);
        // 謙 in EUC-JP is B8 AC... and EB A1 has no CP932 mapping, so only EUC-JP reads this.
        let only_euc = [&b"// "[..], &[0xEB, 0xA1], b"\n"].concat();
        assert_eq!(judge(&only_euc), Encoding::EucJp);
    }

    #[test]
    fn mixed_broken_and_ambiguous() {
        let mixed = [&sjis("// 表示\n")[..], "// 表示\n".as_bytes()].concat();
        assert_eq!(judge(&mixed), Encoding::Mixed);
        let broken = [&b"// "[..], &[0xFF, 0xFE, 0x80], b"\n"].concat();
        assert_eq!(judge(&broken), Encoding::Broken);
        // ﾃｽ in CP932 (C3 BD) is also ý in UTF-8: no Japanese in the UTF-8 reading, so ambiguous.
        assert_eq!(judge(b"// \xC3\xBD\n"), Encoding::AmbiguousUtf8Cp932);
        // 表示する in UTF-8 reads as CP932 too, but its UTF-8 reading is Japanese.
        assert!(cp932::check_line("表示する".as_bytes()).ok);
        assert_eq!(judge("// 表示する\n".as_bytes()), Encoding::Utf8);
        // So does an ellipsis (E2 80 A6: a CP932 pair and ｦ); © (C2 A9: ﾂｩ) alone stays ambiguous.
        assert!(cp932::check_line("a … b".as_bytes()).ok);
        assert_eq!(judge("// a → b\n".as_bytes()), Encoding::Utf8);
        assert_eq!(judge("// © 2026\n".as_bytes()), Encoding::AmbiguousUtf8Cp932);
    }

    #[test]
    fn names_round_trip() {
        for e in Encoding::ALL {
            assert_eq!(e.name().parse::<Encoding>(), Ok(e));
            assert_eq!(serde_json::to_string(&e).unwrap(), format!("\"{}\"", e.name()));
        }
    }
}
