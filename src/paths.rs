//! Windows paths as strings: made absolute without touching the disk, compared without regard to
//! case or slash direction, and mapped from an original root to its mirror.
//!
//! The standard library adds `\\?\` to long absolute paths itself, so a path built here can be
//! passed to `std::fs` whatever its length. `\\?\` is never written into a file mixenc makes
//! (a database, `roots.json`): clangd and editors take the plain form.

use std::path::{Path, PathBuf};

/// Whether `p` is absolute: `C:\…`, `C:/…` or a UNC path.
pub fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || p.starts_with("\\\\")
        || p.starts_with("//")
}

/// `p` made absolute against `base` (itself absolute), with `.` and `..` resolved and backslashes,
/// keeping the case of the letters. A path starting with one slash takes the drive of `base`.
pub fn absolute(base: &str, p: &str) -> String {
    let p = strip_verbatim(p);
    let joined = if is_absolute(p) {
        p.to_string()
    } else if p.starts_with('\\') || p.starts_with('/') {
        format!("{}{}", &strip_verbatim(base)[..2], p)
    } else {
        format!("{}\\{}", strip_verbatim(base), p)
    };
    normalize(&joined)
}

/// Backslashes, no empty, `.` or `..` parts, no trailing backslash (except after a drive).
pub fn normalize(p: &str) -> String {
    let p = strip_verbatim(p).replace('/', "\\");
    let (prefix, rest) = if let Some(rest) = p.strip_prefix("\\\\") {
        // \\server\share stays as the root.
        let mut parts = rest.splitn(3, '\\');
        let server = parts.next().unwrap_or_default();
        let share = parts.next().unwrap_or_default();
        (format!("\\\\{server}\\{share}"), parts.next().unwrap_or_default().to_string())
    } else if p.len() >= 2 && p.as_bytes()[1] == b':' {
        (p[..2].to_ascii_uppercase(), p[2..].to_string())
    } else {
        (String::new(), p.clone())
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() && !prefix.starts_with("\\\\") {
        return format!("{prefix}\\");
    }
    let mut out = prefix;
    for part in parts {
        out.push('\\');
        out.push_str(part);
    }
    out
}

/// The key two paths are equal by on Windows: normalized and lower-case.
pub fn key(p: &str) -> String {
    normalize(p).to_lowercase()
}

/// Whether `p` is `root` or under it (both normalized or not, any case).
pub fn is_under(p: &str, root: &str) -> bool {
    rest_under(p, root).is_some()
}

/// The part of `p` under `root` (without the leading backslash, in `p`'s case), or `None` when `p`
/// is not under `root`.
pub fn rest_under(p: &str, root: &str) -> Option<String> {
    let p = normalize(p);
    let root = normalize(root);
    let (pk, rk) = (p.to_lowercase(), root.to_lowercase());
    // Lower-casing keeps byte lengths for the characters a path holds in practice; check anyway.
    if pk.len() != p.len() || rk.len() != root.len() {
        return None;
    }
    if pk == rk {
        return Some(String::new());
    }
    let rk = rk.trim_end_matches('\\');
    if pk.starts_with(rk) && pk.as_bytes().get(rk.len()) == Some(&b'\\') {
        Some(p[rk.len() + 1..].to_string())
    } else {
        None
    }
}

pub fn strip_verbatim(p: &str) -> &str {
    p.strip_prefix("\\\\?\\").unwrap_or(p)
}

/// The path as the disk spells it: 8.3 short names (`RUNNER~1`) written in full and every part in
/// the disk's case. Links, junctions and `subst` drives are not followed: editors send paths
/// through them, and clangd's `--path-mappings` match the path as sent. The part that does not
/// exist yet is kept as written.
pub fn canonical(p: &Path) -> PathBuf {
    let normalized = PathBuf::from(normalize(&p.to_string_lossy()));
    let mut base = normalized.clone();
    let mut rest = Vec::new();
    loop {
        if let Some(long) = crate::win::long_path_name(&base) {
            // GetLongPathNameW writes short names in full but keeps the case of long ones.
            let mut out = disk_case(Path::new(&normalize(&long.to_string_lossy())));
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (base.file_name().map(|n| n.to_os_string()), base.parent().map(Path::to_path_buf)) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                base = parent;
            }
            _ => return normalized,
        }
    }
}

/// The name of an existing file as its directory lists it.
pub fn disk_name(path: &Path) -> Option<String> {
    crate::win::find_name(path).map(|n| n.to_string_lossy().into_owned())
}

/// An existing path with every part as the directory lists it (one lookup per part).
fn disk_case(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(name) => {
                let real = crate::win::find_name(&out.join(name)).unwrap_or_else(|| name.to_os_string());
                out.push(real);
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `p` normalized, with 8.3 short names written in full when it has any (only a path with `~` can).
/// Two spellings of one folder then compare equal.
pub fn long_form(p: &str) -> String {
    if p.contains('~') { display(&canonical(Path::new(p))) } else { normalize(p) }
}

/// `p` with 8.3 short names where the volume has them (for tests that need such a path).
pub fn short_form(p: &Path) -> PathBuf {
    crate::win::short_path_name(p)
        .map_or_else(|| p.to_path_buf(), |s| PathBuf::from(strip_verbatim(&s.to_string_lossy())))
}

/// A path as a plain string (no `\\?\`), for files mixenc writes and for messages.
pub fn display(p: &Path) -> String {
    strip_verbatim(&p.to_string_lossy()).to_string()
}

/// `root` joined with a relative path that uses `/` (as git prints it).
pub fn join_rel(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        p.push(part);
    }
    p
}

/// The path with forward slashes (clangd's `--path-mappings` and LSP URIs take this form).
pub fn forward(p: &str) -> String {
    strip_verbatim(p).replace('\\', "/")
}

/// A short name for an outside folder's mirror: up to 12 characters of its name and 8 hex digits
/// of a hash of its path, so that two folders with the same name do not meet and the mirror's
/// path stays short.
pub fn short_name(p: &str) -> String {
    use sha2::{Digest, Sha256};
    // Lower-case, so that the same folder written in another case gets the same mirror.
    let key = key(p);
    let name: String = key
        .rsplit('\\')
        .next()
        .unwrap_or("root")
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(12)
        .collect();
    let digest = Sha256::digest(key.as_bytes());
    let hash: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    if name.is_empty() { hash } else { format!("{name}-{hash}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_and_normalize() {
        assert_eq!(absolute(r"C:\repo\src", r"..\inc"), r"C:\repo\inc");
        assert_eq!(absolute(r"C:\repo\src", "inc/./x.h"), r"C:\repo\src\inc\x.h");
        assert_eq!(absolute(r"C:\repo", r"D:/lib\inc\"), r"D:\lib\inc");
        assert_eq!(absolute(r"C:\repo", r"\lib"), r"C:\lib");
        assert_eq!(absolute(r"c:\repo", "."), r"C:\repo");
        assert_eq!(normalize(r"\\?\C:\a\b"), r"C:\a\b");
        assert_eq!(normalize(r"\\server\share\a\..\b"), r"\\server\share\b");
        assert_eq!(normalize("C:/"), r"C:\");
    }

    #[test]
    fn under() {
        assert_eq!(rest_under(r"C:\Repo\Src\A.cpp", r"c:/repo/"), Some(r"Src\A.cpp".into()));
        assert_eq!(rest_under(r"C:\repo", r"C:\Repo"), Some(String::new()));
        assert_eq!(rest_under(r"C:\repo2\a", r"C:\repo"), None);
        assert!(is_under(r"C:\ws\repos\表示\a.h", r"C:\ws\repos"));
    }

    #[test]
    fn canonical_spells_as_the_disk_does() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("Case Dir With A Long Name");
        std::fs::create_dir(&real).unwrap();
        let typed = dir.path().join("case dir with a long name").join("not yet");
        assert_eq!(canonical(&typed), canonical(&real).join("not yet"));
        assert!(canonical(&typed).to_string_lossy().contains("Case Dir With A Long Name"));
        // Where the volume makes 8.3 names, the short spelling comes back long.
        let short = short_form(&real).display().to_string();
        assert_eq!(long_form(&short), display(&canonical(&real)), "{short}");
    }

    #[test]
    fn short_names() {
        let a = short_name(r"C:\Vendor\SDK\include");
        assert!(a.starts_with("include-") && a.len() == "include-".len() + 8, "{a}");
        assert_eq!(short_name(r"C:\Vendor\SDK\Include"), a);
        assert_ne!(a, short_name(r"D:\Other\include"));
        assert_eq!(a, short_name(r"c:/vendor/sdk/INCLUDE/"));
        assert_eq!(short_name(r"C:\a_very_long_folder_name").len(), 12 + 1 + 8);
    }
}
