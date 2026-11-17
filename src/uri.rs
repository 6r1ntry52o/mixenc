//! `file:` URIs as editors write them on Windows (`file:///c%3A/src/a.cpp`).

use std::path::PathBuf;

/// The path of a `file:` URI, `None` for another scheme.
pub fn to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest)?;
    let path = match decoded.strip_prefix('/') {
        // file:///C:/a → C:/a
        Some(p) if p.as_bytes().get(1) == Some(&b':') => p.to_string(),
        Some(p) => format!("/{p}"),
        // file://server/share/a → //server/share/a
        None => format!("//{decoded}"),
    };
    Some(PathBuf::from(path.replace('/', "\\")))
}

/// The `file:` URI of an absolute path (`file:///C:/src/a%20b.cpp`).
pub fn from_path(path: &std::path::Path) -> String {
    let p = crate::paths::forward(&path.to_string_lossy());
    let mut out = String::from(if p.starts_with("//") { "file:" } else { "file:///" });
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_uris() {
        assert_eq!(to_path("file:///c%3A/src/a.cpp"), Some(PathBuf::from(r"c:\src\a.cpp")));
        assert_eq!(to_path("file:///C:/src/%E8%A1%A8.h"), Some(PathBuf::from(r"C:\src\表.h")));
        assert_eq!(to_path("file://server/share/a.h"), Some(PathBuf::from(r"\\server\share\a.h")));
        assert_eq!(to_path("untitled:Untitled-1"), None);
        assert_eq!(to_path("file:///c%3A/a%20b/c%25"), Some(PathBuf::from(r"c:\a b\c%")));
    }

    #[test]
    fn paths_to_uris_and_back() {
        for p in [r"C:\src\表 1.h", r"\\server\share\a#b.h", r"C:\a%b\c.cpp"] {
            let uri = from_path(std::path::Path::new(p));
            assert_eq!(to_path(&uri), Some(PathBuf::from(p)), "{uri}");
        }
        assert_eq!(from_path(std::path::Path::new(r"C:\a b\c.cpp")), "file:///C:/a%20b/c.cpp");
    }
}
