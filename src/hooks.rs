//! The commit gate: a git pre-commit hook that runs [`crate::gate`] on every staged file and stops
//! the commit when one breaks invariant 1 (its encoding changed, a line whose text did not change
//! got other bytes, a mixed or broken file got worse). It only stops: it never changes a commit.
//! People's saves and agents' writes meet it alike.
//!
//! Installing it touches nothing outside the clone (git does not push `.git/hooks`):
//! - a pre-commit already there is kept (renamed `pre-commit.before-mixenc`) and runs first; the
//!   gate looks only when it passed;
//! - with `core.hooksPath` set (hooks managed by something else), nothing is installed and the
//!   doctor says so;
//! - `mixenc hooks uninstall` puts the old hook back; `git commit --no-verify` skips both.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::config::Workspace;
use crate::defaults::{Defaults, Pick};
use crate::gate::{self, Check};
use crate::paths;

/// The line that marks a pre-commit as mixenc's.
pub const MARKER: &str = "# mixenc pre-commit";
/// What the pre-commit that was there before is renamed to.
pub const BEFORE: &str = "pre-commit.before-mixenc";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// mixenc's pre-commit is in place; `chained` when the one before it runs first.
    Installed { chained: bool },
    /// No pre-commit of mixenc's; `other` when there is another one.
    Missing { other: bool },
    /// `core.hooksPath` is set: the hooks live there, and mixenc does not install into them.
    HooksPath(String),
}

pub fn status(repo: &Path) -> Result<Status> {
    if let Some(p) = hooks_path(repo) {
        return Ok(Status::HooksPath(p));
    }
    let dir = hooks_dir(repo)?;
    let hook = dir.join("pre-commit");
    Ok(if is_ours(&hook) {
        Status::Installed { chained: dir.join(BEFORE).exists() }
    } else {
        Status::Missing { other: hook.exists() }
    })
}

/// Installs the pre-commit that runs `exe` (written again when it is there already).
pub fn install(repo: &Path, exe: &Path) -> Result<Status> {
    if let Some(p) = hooks_path(repo) {
        return Ok(Status::HooksPath(p));
    }
    let dir = hooks_dir(repo)?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", paths::display(&dir)))?;
    let hook = dir.join("pre-commit");
    let before = dir.join(BEFORE);
    if hook.exists() && !is_ours(&hook) {
        if before.exists() {
            bail!("{} and {} are both there; move one away first", paths::display(&hook), paths::display(&before));
        }
        fs::rename(&hook, &before).with_context(|| format!("renaming {}", paths::display(&hook)))?;
    }
    fs::write(&hook, script(exe)).with_context(|| format!("writing {}", paths::display(&hook)))?;
    status(repo)
}

/// Removes mixenc's pre-commit and puts back the one that was there before.
pub fn uninstall(repo: &Path) -> Result<Status> {
    if let Some(p) = hooks_path(repo) {
        return Ok(Status::HooksPath(p));
    }
    let dir = hooks_dir(repo)?;
    let hook = dir.join("pre-commit");
    if is_ours(&hook) {
        fs::remove_file(&hook).with_context(|| format!("removing {}", paths::display(&hook)))?;
        let before = dir.join(BEFORE);
        if before.exists() {
            fs::rename(&before, &hook).with_context(|| format!("renaming {}", paths::display(&before)))?;
        }
    }
    status(repo)
}

fn is_ours(hook: &Path) -> bool {
    fs::read(hook).is_ok_and(|bytes| String::from_utf8_lossy(&bytes).lines().any(|l| l.starts_with(MARKER)))
}

/// `core.hooksPath`, when set.
fn hooks_path(repo: &Path) -> Option<String> {
    let output = Command::new("git").args(["config", "--get", "core.hooksPath"]).current_dir(repo).output().ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !value.is_empty()).then_some(value)
}

/// The clone's hooks folder (shared by its worktrees).
fn hooks_dir(repo: &Path) -> Result<PathBuf> {
    let out = git(repo, &["rev-parse", "--git-path", "hooks"])?;
    let dir = String::from_utf8_lossy(&out).trim().to_string();
    Ok(PathBuf::from(paths::absolute(&repo.to_string_lossy(), &dir)))
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git").args(args).current_dir(dir).output().context("running git (is it on PATH?)")?;
    if !output.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(output.stdout)
}

/// The hook: the one that was there before, then `exe hooks pre-commit` (or `mixenc` on PATH).
fn script(exe: &Path) -> String {
    let exe = paths::forward(&exe.to_string_lossy()).replace('\'', r"'\''");
    format!(
        r#"#!/bin/sh
{MARKER}: the commit gate. It stops a commit that changes a file's encoding or the bytes of
# lines whose text did not change; it never changes a commit. `mixenc hooks uninstall` removes it
# and puts back the hook that was here before ({BEFORE}), if any.
hooks=$(dirname "$0")
if [ -f "$hooks/{BEFORE}" ]; then
    "$hooks/{BEFORE}" "$@" || exit $?
fi
mixenc='{exe}'
if [ ! -f "$mixenc" ]; then
    mixenc=$(command -v mixenc) || {{
        echo "mixenc pre-commit: mixenc is not found, so the commit gate cannot check this commit." >&2
        echo "Install mixenc, remove the gate (mixenc hooks uninstall), or commit with --no-verify." >&2
        exit 1
    }}
fi
exec "$mixenc" hooks pre-commit
"#
    )
}

/// A staged change of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Relative to the top of the work tree, with `/`.
    pub path: String,
    /// The blob in HEAD (`None` for a new file).
    pub old: Option<String>,
    pub new: String,
}

/// The staged files that were added, modified, renamed or copied (not submodules or links).
pub fn staged(top: &Path) -> Result<Vec<Change>> {
    let out = git(
        top,
        &["-c", "core.quotepath=off", "diff", "--cached", "--raw", "-z", "--no-abbrev", "-M", "--diff-filter=ACMR"],
    )?;
    Ok(parse_raw(&out))
}

fn parse_raw(out: &[u8]) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut fields = out.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(header) = fields.next() {
        let header = String::from_utf8_lossy(header);
        let parts: Vec<&str> = header.trim_start_matches(':').split(' ').collect();
        let [old_mode, new_mode, old_sha, new_sha, status] = parts[..] else { continue };
        let renamed = status.starts_with('R') || status.starts_with('C');
        let Some(mut path) = fields.next() else { break };
        if renamed {
            match fields.next() {
                Some(new_path) => path = new_path,
                None => break,
            }
        }
        let file_mode = |m: &str| !matches!(m, "160000" | "120000");
        if !file_mode(new_mode) || !file_mode(old_mode) {
            continue;
        }
        let zero = old_sha.bytes().all(|b| b == b'0');
        changes.push(Change {
            path: String::from_utf8_lossy(path).into_owned(),
            old: (!zero).then(|| old_sha.to_string()),
            new: new_sha.to_string(),
        });
    }
    changes
}

/// Blobs read through one `git cat-file --batch`.
struct Blobs {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Blobs {
    fn start(top: &Path) -> Result<Blobs> {
        let mut child = Command::new("git")
            .args(["cat-file", "--batch"])
            .current_dir(top)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("running git cat-file")?;
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().context("git cat-file's output")?);
        Ok(Blobs { child, input, output })
    }

    fn read(&mut self, sha: &str) -> Result<Vec<u8>> {
        let input = self.input.as_mut().context("git cat-file's input")?;
        writeln!(input, "{sha}")?;
        input.flush()?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        let size = match header.trim_end().split(' ').collect::<Vec<_>>()[..] {
            [_, "blob", size] => size.parse::<usize>()?,
            _ => bail!("git cat-file {sha}: {}", header.trim_end()),
        };
        let mut blob = vec![0; size + 1];
        self.output.read_exact(&mut blob)?;
        blob.pop();
        Ok(blob)
    }
}

impl Drop for Blobs {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.child.wait();
    }
}

/// What the gate found in one staged file.
#[derive(Clone, Debug)]
pub struct Stopped {
    pub path: String,
    pub check: Check,
}

/// Checks the staged files of the repository holding `dir`; returns those the gate stops.
pub fn check_staged(dir: &Path) -> Result<Vec<Stopped>> {
    let top = String::from_utf8_lossy(&git(dir, &["rev-parse", "--show-toplevel"])?).trim().to_string();
    let top = paths::canonical(Path::new(&paths::normalize(&top)));
    let ws = Workspace::find(&top).ok();
    let repo = ws.as_ref().and_then(|ws| {
        ws.repos().into_iter().find(|r| paths::key(&r.source.to_string_lossy()) == paths::key(&top.to_string_lossy()))
    });
    let changes = staged(&top)?;
    let mut blobs = Blobs::start(&top)?;
    let mut defaults: Option<Defaults> = None;
    let mut stopped = Vec::new();
    for change in changes {
        let old = change.old.as_deref().map(|sha| blobs.read(sha)).transpose()?;
        let new = blobs.read(&change.new)?;
        let abs = paths::join_rel(&top, &change.path);
        let read_as = ws.as_ref().and_then(|ws| ws.override_for(&abs)).map(|o| o.read_as);
        let mut pick = || match (&ws, &repo) {
            (Some(ws), Some(repo)) => defaults.get_or_insert_with(|| Defaults::load(ws, repo)).for_file(&change.path),
            _ => Pick::fallback(),
        };
        let check = gate::check(old.as_deref(), &new, read_as, &mut pick);
        if !check.passed() {
            stopped.push(Stopped { path: change.path, check });
        }
    }
    Ok(stopped)
}

/// The message for a stopped commit.
pub fn report(stopped: &[Stopped]) -> String {
    let mut out = String::from("mixenc pre-commit: the commit gate stopped this commit.\n");
    for s in stopped {
        let encodings = match s.check.old {
            Some(old) if old != s.check.new => format!(" ({old} -> {})", s.check.new),
            Some(_) => format!(" ({})", s.check.new),
            None => format!(" (new, {})", s.check.new),
        };
        out.push_str(&format!("  {}{encodings}\n", s.path));
        for f in &s.check.findings {
            match f.line {
                Some(line) => out.push_str(&format!("    line {line}: {}\n", f.message)),
                None => out.push_str(&format!("    {}\n", f.message)),
            }
        }
        if s.check.more_lines > 0 {
            out.push_str(&format!("    ... and {} more lines like these\n", s.check.more_lines));
        }
    }
    out.push_str(
        "Write such files back as they were in the lines you did not mean to change (mixenc's edit tool keeps\n\
         each line's bytes), or commit anyway with git commit --no-verify.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_raw_diff() {
        let z = "0".repeat(40);
        let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let out = format!(
            ":000000 100644 {z} {a} A\0new.h\0:100644 100644 {a} {b} M\0src/表.cpp\0:100644 100644 {b} {c} R091\0old.h\0moved.h\0:160000 160000 {a} {b} M\0sub\0"
        );
        let changes = parse_raw(out.as_bytes());
        assert_eq!(
            changes,
            [
                Change { path: "new.h".into(), old: None, new: a.clone() },
                Change { path: "src/表.cpp".into(), old: Some(a.clone()), new: b.clone() },
                Change { path: "moved.h".into(), old: Some(b), new: c },
            ]
        );
    }

    #[test]
    fn the_script_runs_the_old_hook_first() {
        let s = script(Path::new(r"C:\Program Files\it's\mixenc.exe"));
        assert!(s.starts_with("#!/bin/sh\n"));
        assert!(s.contains(&format!("\n{MARKER}:")));
        assert!(s.contains(r#"mixenc='C:/Program Files/it'\''s/mixenc.exe'"#), "{s}");
        assert!(s.find(BEFORE).unwrap() < s.find("exec").unwrap());
        assert!(!s.contains('\r'));
    }
}
