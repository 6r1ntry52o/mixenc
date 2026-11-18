//! File operations on mirrors. A mirror file is written to a temporary file first and renamed into
//! place, so a reader (clangd) never sees half of it, and it is left read-only, so an editor that
//! opens it by mistake cannot save over it.

use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use sha2::{Digest, Sha256};

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The modification time in nanoseconds since 1970 (0 when unknown).
pub fn mtime(meta: &Metadata) -> u64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as u64)
}

/// A temporary file's path in `tmp_dir`: short and fixed in form, so that it never makes a path or
/// a name longer than the files it stands for.
pub fn temp_path(tmp_dir: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    tmp_dir.join(format!("{:x}-{:x}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))
}

/// Writes `bytes` to `target` through a temporary file in `tmp_dir` (on the same volume), and
/// leaves `target` read-only when `read_only`.
pub fn write_replace(tmp_dir: &Path, target: &Path, bytes: &[u8], read_only: bool) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(tmp_dir)?;
    let tmp = temp_path(tmp_dir);
    fs::write(&tmp, bytes)?;
    if read_only {
        set_read_only(&tmp, true)?;
    }
    let result = rename_over(&tmp, target);
    if result.is_err() {
        let _ = set_read_only(&tmp, false);
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Renames `from` over `to`, which may be read-only or briefly open in another process.
fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(20 << attempt));
        }
        if let Ok(meta) = fs::symlink_metadata(to)
            && meta.permissions().readonly()
        {
            set_read_only(to, false)?;
        }
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) => last = Some(err),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
}

/// Removes a mirror file (read-only or not); a file that is not there is fine.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.permissions().readonly() {
                set_read_only(path, false)?;
            }
            fs::remove_file(path)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Removes the empty folders from `dir` up to (not including) `stop`.
pub fn remove_empty_parents(dir: &Path, stop: &Path) {
    let mut dir = dir.to_path_buf();
    while dir.starts_with(stop) && dir != stop {
        if fs::remove_dir(&dir).is_err() {
            break;
        }
        if !dir.pop() {
            break;
        }
    }
}

pub fn set_read_only(path: &Path, read_only: bool) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    if permissions.readonly() != read_only {
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(read_only);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

/// The files under `root`, as paths relative to it with `/`, skipping the folders `skip` says.
pub fn walk(root: &Path, skip: &dyn Fn(&str) -> bool) -> io::Result<Vec<String>> {
    walk_with(root, &|rel, _| skip(rel))
}

/// [`walk`], with whether the entry is a folder.
pub fn walk_with(root: &Path, skip: &dyn Fn(&str, bool) -> bool) -> io::Result<Vec<String>> {
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if dir != root && err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if !skip(&child, true) {
                    stack.push((entry.path(), child));
                }
            } else if !skip(&child, false) {
                files.push(child);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_replace_over_a_read_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a").join("b.h");
        let tmp = dir.path().join("tmp");
        write_replace(&tmp, &target, b"one", true).unwrap();
        assert!(fs::metadata(&target).unwrap().permissions().readonly());
        write_replace(&tmp, &target, b"two", true).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0);
        remove(&target).unwrap();
        remove(&target).unwrap();
        remove_empty_parents(&dir.path().join("a"), dir.path());
        assert!(!dir.path().join("a").exists());
    }

    #[test]
    fn walk_lists_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("x/y")).unwrap();
        fs::create_dir_all(dir.path().join("skip")).unwrap();
        fs::write(dir.path().join("x/y/a.h"), "").unwrap();
        fs::write(dir.path().join("b.h"), "").unwrap();
        fs::write(dir.path().join("skip/c.h"), "").unwrap();
        assert_eq!(walk(dir.path(), &|rel| rel == "skip").unwrap(), ["b.h", "x/y/a.h"]);
    }
}
