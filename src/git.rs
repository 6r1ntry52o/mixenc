//! What git says about a repository.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::paths;

/// The files of the repository as paths relative to it with `/`: the tracked ones (in submodules
/// too) and the untracked ones that are not ignored (a new header not added yet is still included
/// by the sources). Files the index has but the disk has not are left to the caller.
pub fn list_files(repo: &Path) -> Result<Vec<String>> {
    let mut files = ls_files(repo, &["--cached", "--recurse-submodules"])?;
    files.extend(ls_files(repo, &["--others", "--exclude-standard"])?);
    files.sort();
    files.dedup();
    Ok(files)
}

fn ls_files(repo: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["-c", "core.quotepath=off", "ls-files", "-z"])
        .args(args)
        .current_dir(repo)
        .output()
        .context("running git (is it on PATH?)")?;
    if !output.status.success() {
        bail!("git ls-files in {}: {}", paths::display(repo), String::from_utf8_lossy(&output.stderr).trim());
    }
    // git prints paths in UTF-8 on Windows.
    Ok(output
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

/// A boolean setting as git reads it for the repository (`None` when unset or unreadable).
pub fn config_bool(repo: &Path, name: &str) -> Option<bool> {
    let output = Command::new("git").args(["config", "--type=bool", "--get", name]).current_dir(repo).output().ok()?;
    match String::from_utf8_lossy(&output.stdout).trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}
