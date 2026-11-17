//! Lines of bytes. A line ends at LF, CRLF or a lone CR, as clang and MSVC read it. None of these
//! bytes can be the second byte of a CP932 character (those are 0x40 and above), so the lines of a
//! file are the same whichever encoding it is read in, except UTF-16.

pub const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// The lines of `bytes`, each as its content and its line break (empty for a last line without
/// one). A file ending with a line break has no empty line after it.
pub fn split_lines(bytes: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut lines = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        let end = match bytes[i] {
            b'\n' => i + 1,
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => i + 2,
            b'\r' => i + 1,
            _ => {
                i += 1;
                continue;
            }
        };
        lines.push((&bytes[start..i], &bytes[i..end]));
        i = end;
        start = end;
    }
    if start < bytes.len() {
        lines.push((&bytes[start..], &bytes[bytes.len()..]));
    }
    lines
}

/// The number of lines of UTF-16 code units, counted as [`split_lines`] counts bytes.
pub fn count_lines_utf16(units: &[u16]) -> usize {
    let mut count = 0;
    let mut i = 0;
    let mut open = false;
    while i < units.len() {
        match units[i] {
            0x0A => {
                count += 1;
                open = false;
            }
            0x0D => {
                count += 1;
                open = false;
                if units.get(i + 1) == Some(&0x0A) {
                    i += 1;
                }
            }
            _ => open = true,
        }
        i += 1;
    }
    count + usize::from(open)
}

/// The line without the spaces, tabs, vertical tabs and form feeds at its end.
pub fn trim_end_blank(line: &[u8]) -> &[u8] {
    let end = line.iter().rposition(|b| !matches!(b, b' ' | b'\t' | 0x0B | 0x0C)).map_or(0, |i| i + 1);
    &line[..end]
}

/// Whether the UTF-8 (or ASCII-compatible) line ends with a backslash, allowing blanks after it.
pub fn ends_with_backslash(line: &[u8]) -> bool {
    trim_end_blank(line).last() == Some(&b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_breaks() {
        let lines = split_lines(b"a\r\nb\nc\rd");
        assert_eq!(
            lines,
            [(&b"a"[..], &b"\r\n"[..]), (&b"b"[..], &b"\n"[..]), (&b"c"[..], &b"\r"[..]), (&b"d"[..], &b""[..])]
        );
        assert_eq!(split_lines(b"a\n").len(), 1);
        assert_eq!(split_lines(b"").len(), 0);
        assert_eq!(split_lines(b"\n\n").len(), 2);
        let utf16: Vec<u16> = "a\r\nb\nc\rd".encode_utf16().collect();
        assert_eq!(count_lines_utf16(&utf16), 4);
        let utf16: Vec<u16> = "a\n\n".encode_utf16().collect();
        assert_eq!(count_lines_utf16(&utf16), 2);
    }

    #[test]
    fn backslash_at_the_end() {
        assert!(ends_with_backslash(b"#define A \\ \t"));
        assert!(!ends_with_backslash(b"a\\b"));
        assert_eq!(trim_end_blank(b"   "), b"");
    }
}
