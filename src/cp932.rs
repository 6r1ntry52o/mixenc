//! Windows code page 932 (Shift_JIS with the NEC and IBM extensions), read the way MSVC reads a
//! source file: a lead byte takes the next byte with it, whatever that byte is. So a `0x5C` after a
//! lead byte (表 is `95 5C`) is never a backslash, and a pair with no mapping becomes one `U+FFFD`
//! for both bytes, not a `U+FFFD` and a backslash (what Python's `errors="replace"` and the WHATWG
//! decoder give, which turns a comment ending in such a pair into a line splice).
//!
//! The pairs are mapped with Windows' own table (`MultiByteToWideChar`), so mixenc and Visual Studio
//! agree on every character. `0x80`, which Windows maps to U+0080, is treated as invalid: it is
//! not Japanese text, and a file holding it is more likely Windows-1252 or broken.

use std::sync::OnceLock;

use crate::text::trim_end_blank;

pub const REPLACEMENT: char = '\u{FFFD}';

/// Whether `b` starts a two-byte character (`IsDBCSLeadByteEx(932, b)`).
pub fn is_lead(b: u8) -> bool {
    matches!(b, 0x81..=0x9F | 0xE0..=0xFC)
}

/// One unit of a CP932 line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// An ASCII byte or a half-width katakana.
    Single(u8),
    /// A lead byte and the byte after it.
    Pair(u8, u8),
    /// A byte that cannot start a character (`0x80`, `0xA0`, `0xFD`-`0xFF`), or a lead byte at the
    /// end of the line.
    Invalid(u8),
}

/// Splits a line (without its line break) into units.
pub fn units(line: &[u8]) -> impl Iterator<Item = Unit> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        let b = *line.get(i)?;
        i += 1;
        Some(match b {
            0x00..=0x7F | 0xA1..=0xDF => Unit::Single(b),
            _ if is_lead(b) => match line.get(i) {
                Some(&trail) => {
                    i += 1;
                    Unit::Pair(b, trail)
                }
                None => Unit::Invalid(b),
            },
            _ => Unit::Invalid(b),
        })
    })
}

/// The character of a pair, if Windows maps it.
pub fn pair(lead: u8, trail: u8) -> Option<char> {
    let index = lead_index(lead)? * 256 + trail as usize;
    match table()[index] {
        0 => None,
        c => char::from_u32(u32::from(c)),
    }
}

/// What reading a line as CP932 found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineCheck {
    /// Every unit has a mapping.
    pub ok: bool,
    /// A pair starts with `0x81`-`0x9F` (kana and most symbols), which EUC-JP text never has.
    pub low_lead: bool,
}

pub fn check_line(line: &[u8]) -> LineCheck {
    let mut check = LineCheck { ok: true, low_lead: false };
    for unit in units(line) {
        match unit {
            Unit::Single(_) => {}
            Unit::Pair(lead, trail) => {
                check.low_lead |= lead <= 0x9F;
                check.ok &= pair(lead, trail).is_some();
            }
            Unit::Invalid(_) => check.ok = false,
        }
    }
    check
}

/// Appends the line (without its line break) to `out`, a unit without a mapping as one `U+FFFD`.
/// Returns whether every unit had a mapping.
pub fn decode_line(line: &[u8], out: &mut String) -> bool {
    if line.is_ascii() {
        // ASCII is valid UTF-8.
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        return true;
    }
    let mut ok = true;
    for unit in units(line) {
        let c = match unit {
            Unit::Single(b) if b < 0x80 => char::from(b),
            // Half-width katakana: A1-DF is U+FF61-U+FF9F.
            Unit::Single(b) => char::from_u32(0xFF61 + u32::from(b - 0xA1)).unwrap_or(REPLACEMENT),
            Unit::Pair(lead, trail) => pair(lead, trail).unwrap_or_else(|| {
                ok = false;
                REPLACEMENT
            }),
            Unit::Invalid(_) => {
                ok = false;
                REPLACEMENT
            }
        };
        out.push(c);
    }
    ok
}

/// Whether the line, read as CP932, ends with a backslash, allowing spaces and tabs after it (clang
/// and MSVC splice such a line too).
pub fn ends_with_backslash(line: &[u8]) -> bool {
    matches!(units(trim_end_blank(line)).last(), Some(Unit::Single(b'\\')))
}

/// Whether the line's last byte (before spaces and tabs) is `0x5C` that CP932 reads as the second
/// byte of a character: the line a UTF-8 reader would splice with the next one.
pub fn is_trap(line: &[u8]) -> bool {
    trim_end_blank(line).last() == Some(&b'\\') && !ends_with_backslash(line)
}

fn lead_index(lead: u8) -> Option<usize> {
    match lead {
        0x81..=0x9F => Some(usize::from(lead - 0x81)),
        0xE0..=0xFC => Some(usize::from(lead - 0xE0) + 31),
        _ => None,
    }
}

/// Windows' mapping of every pair, by `lead_index * 256 + trail`; 0 where there is none.
fn table() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = vec![0u16; 60 * 256];
        for lead in (0x81..=0x9Fu8).chain(0xE0..=0xFC) {
            let base = lead_index(lead).unwrap_or_default() * 256;
            for trail in 0..=255u8 {
                if let Some(c) = crate::win::cp932_pair(lead, trail) {
                    table[base + usize::from(trail)] = c;
                }
            }
        }
        table
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> (String, bool) {
        let mut s = String::new();
        let ok = decode_line(bytes, &mut s);
        (s, ok)
    }

    #[test]
    fn a_trail_byte_5c_is_part_of_the_character() {
        // "// 表" ending the line: 表 is 95 5C.
        assert_eq!(decode(b"// \x95\x5C"), ("// 表".into(), true));
        assert!(ends_with_backslash(b"#define A \\"));
        assert!(!ends_with_backslash(b"// \x95\x5C"));
        assert!(is_trap(b"// \x94\x5C  "), "能 with trailing spaces");
        assert!(is_trap(b"// \x83\x5C"), "ソ");
        assert!(!is_trap(b"#define A \\"));
    }

    #[test]
    fn an_unmapped_pair_is_one_replacement_character() {
        // 85 5C has a valid lead byte but no mapping: the backslash goes with it.
        assert_eq!(decode(b"// \x85\x5C"), ("// \u{FFFD}".into(), false));
        assert!(!ends_with_backslash(b"// \x85\x5C"));
        // FD cannot start a character, so the 5C after it is a backslash (as MSVC reads it).
        assert_eq!(decode(b"x\xFD\x5C"), ("x\u{FFFD}\\".into(), false));
        assert!(ends_with_backslash(b"x\xFD\x5C"));
    }

    #[test]
    fn a_lead_byte_cut_at_the_end_of_the_line() {
        assert_eq!(decode(b"abc\x82"), ("abc\u{FFFD}".into(), false));
        assert_eq!(check_line(b"abc\x82"), LineCheck { ok: false, low_lead: false });
    }

    #[test]
    fn nec_and_ibm_forms_of_the_same_character() {
        // Ⅰ: NEC row 13 (87 54) and IBM extension (FA 4A); ⅰ: NEC-selected IBM (EE EF).
        assert_eq!(decode(b"\x87\x54\xFA\x4A\xEE\xEF").0, "ⅠⅠⅰ");
        // ∵: JIS (81 E6) and NEC (87 9A).
        assert_eq!(decode(b"\x81\xE6\x87\x9A").0, "∵∵");
    }

    #[test]
    fn half_width_katakana_and_user_defined_area() {
        assert_eq!(decode(b"\xB1\xDF").0, "ｱﾟ");
        assert_eq!(decode(b"\xF0\x40").0, "\u{E000}");
        assert_eq!(decode(b"\x80"), ("\u{FFFD}".into(), false));
    }

    #[test]
    fn low_lead_bytes() {
        assert!(check_line(b"\x82\xA0").low_lead);
        assert!(!check_line(b"\xE4\xA2").low_lead);
    }
}
