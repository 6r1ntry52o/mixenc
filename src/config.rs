//! `mixenc.json`, the workspace's one settings file. The folder that holds it is the workspace, and
//! the mirrors go to its `mirror` folder:
//!
//! ```json
//! {
//!   "format_version": 1,
//!   "repos": [{ "path": "repos/cloneA", "wiki": "repos/cloneA_Wiki" }],
//!   "external": ["C:/Vendor/SDK/include"],
//!   "overrides": { "repos/cloneA/src/old.h": { "as": "cp932", "judged": "ambiguous-cp932-euc-jp" } }
//! }
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::judge::Encoding;
use crate::paths;

pub const FILE_NAME: &str = "mixenc.json";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "format_version")]
    pub format_version: u32,
    #[serde(default)]
    pub repos: Vec<Repo>,
    /// Include folders outside the repositories to mirror, besides those the databases name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external: Vec<String>,
    /// Encodings a person decided, by path: relative to the workspace with `/`, or absolute for a
    /// file outside it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, Override>,
}

fn format_version() -> u32 {
    FORMAT_VERSION
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repo {
    /// The repository's folder, relative to the workspace or absolute.
    pub path: String,
    /// The name of its mirror folder (default: the repository folder's name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its documents, opened beside it in the editor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki: Option<String>,
    /// Its `compile_commands.json` (the file or its folder). Default: where lazymsbuild publishes
    /// it, else the repository's root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Override {
    #[serde(rename = "as")]
    pub read_as: ReadAs,
    /// The judgement when the override was written; the doctor lists the override once it changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged: Option<Encoding>,
}

/// What an override says: read the file as this encoding, or leave it out of the mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadAs {
    Skip,
    Encoding(Encoding),
}

impl Serialize for ReadAs {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            ReadAs::Skip => s.serialize_str("skip"),
            ReadAs::Encoding(e) => s.serialize_str(e.name()),
        }
    }
}

impl<'de> Deserialize<'de> for ReadAs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s == "skip" {
            return Ok(ReadAs::Skip);
        }
        let e: Encoding = s.parse().map_err(serde::de::Error::custom)?;
        if matches!(e, Encoding::AmbiguousUtf8Cp932 | Encoding::AmbiguousCp932EucJp | Encoding::Binary) {
            return Err(serde::de::Error::custom(format!("`{s}` is a judgement, not an encoding to read a file as")));
        }
        Ok(ReadAs::Encoding(e))
    }
}

/// A loaded workspace.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub config: Config,
    /// The overrides by path key.
    overrides: BTreeMap<String, (String, Override)>,
}

/// A repository with its paths resolved.
#[derive(Clone, Debug)]
pub struct RepoPaths {
    pub name: String,
    pub source: PathBuf,
    pub mirror: PathBuf,
    pub wiki: Option<PathBuf>,
    pub db: Option<PathBuf>,
}

impl Workspace {
    /// The workspace holding `start` (the nearest folder upward with a `mixenc.json`).
    pub fn find(start: &Path) -> Result<Workspace> {
        let start = std::path::absolute(start)?;
        for dir in start.ancestors() {
            if dir.join(FILE_NAME).is_file() {
                return Workspace::load(dir);
            }
        }
        bail!("no {FILE_NAME} in {} or above it (mixenc init writes one)", paths::display(&start))
    }

    pub fn load(root: &Path) -> Result<Workspace> {
        let file = root.join(FILE_NAME);
        let text = fs::read_to_string(&file).with_context(|| format!("reading {}", paths::display(&file)))?;
        let config: Config =
            serde_json::from_str(&text).with_context(|| format!("reading {}", paths::display(&file)))?;
        if config.format_version > FORMAT_VERSION {
            bail!(
                "{} is format {} and this mixenc reads up to {FORMAT_VERSION}",
                paths::display(&file),
                config.format_version
            );
        }
        Workspace::new(root, config)
    }

    pub fn new(root: &Path, config: Config) -> Result<Workspace> {
        let root = paths::canonical(&std::path::absolute(root)?);
        let mut overrides = BTreeMap::new();
        for (path, o) in &config.overrides {
            let abs = paths::absolute(&root.to_string_lossy(), path);
            overrides.insert(paths::key(&abs), (path.clone(), o.clone()));
        }
        let workspace = Workspace { root, config, overrides };
        let mut names = BTreeMap::new();
        for repo in workspace.repos() {
            if let Some(other) = names.insert(repo.name.to_lowercase(), repo.source.clone()) {
                bail!(
                    "two repositories mirror to `{}`: {} and {} (give one a `name`)",
                    repo.name,
                    paths::display(&other),
                    paths::display(&repo.source)
                );
            }
            if paths::is_under(&workspace.mirror_root().to_string_lossy(), &repo.source.to_string_lossy()) {
                bail!(
                    "the mirror folder {} is inside the repository {}",
                    paths::display(&workspace.mirror_root()),
                    paths::display(&repo.source)
                );
            }
        }
        Ok(workspace)
    }

    pub fn file(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    pub fn mirror_root(&self) -> PathBuf {
        self.root.join("mirror")
    }

    /// Where the mirrors of the include folders outside the repositories go.
    pub fn external_root(&self) -> PathBuf {
        self.mirror_root().join("_ext")
    }

    /// The workspace's own state (the toolchain scan).
    pub fn state_dir(&self) -> PathBuf {
        self.mirror_root().join(crate::index::STATE_DIR)
    }

    pub fn resolve(&self, p: &str) -> PathBuf {
        PathBuf::from(paths::absolute(&self.root.to_string_lossy(), p))
    }

    pub fn repos(&self) -> Vec<RepoPaths> {
        self.config
            .repos
            .iter()
            .map(|r| {
                let source = paths::canonical(&self.resolve(&r.path));
                let name = r.name.clone().unwrap_or_else(|| {
                    source.file_name().map_or_else(|| "repo".to_string(), |n| n.to_string_lossy().into_owned())
                });
                RepoPaths {
                    mirror: self.mirror_root().join(&name),
                    name,
                    source,
                    wiki: r.wiki.as_deref().map(|w| self.resolve(w)),
                    db: r.db.as_deref().map(|d| self.resolve(d)),
                }
            })
            .collect()
    }

    /// The override for a file, by its absolute path.
    pub fn override_for(&self, abs: &Path) -> Option<&Override> {
        self.overrides.get(&paths::key(&abs.to_string_lossy())).map(|(_, o)| o)
    }

    /// The overrides with their absolute paths, as written in the file.
    pub fn overrides(&self) -> impl Iterator<Item = (&str, PathBuf, &Override)> {
        self.overrides.values().map(|(path, o)| (path.as_str(), self.resolve(path), o))
    }

    /// How `mixenc.json` names a file: relative to the workspace with `/`, else absolute.
    pub fn display_path(&self, abs: &Path) -> String {
        let s = abs.to_string_lossy();
        match paths::rest_under(&s, &self.root.to_string_lossy()) {
            Some(rest) => rest.replace('\\', "/"),
            None => paths::forward(&s),
        }
    }
}

/// Writes a new `mixenc.json` in `root` for the repositories (and their documents).
pub fn init(root: &Path, repos: &[String], wiki: Option<&str>) -> Result<PathBuf> {
    let file = root.join(FILE_NAME);
    if file.exists() {
        bail!("{} exists already", paths::display(&file));
    }
    let config = Config {
        format_version: FORMAT_VERSION,
        repos: repos
            .iter()
            .enumerate()
            .map(|(i, path)| Repo {
                path: relative_to(root, path),
                name: None,
                wiki: if i == 0 { wiki.map(|w| relative_to(root, w)) } else { None },
                db: None,
            })
            .collect(),
        external: Vec::new(),
        overrides: BTreeMap::new(),
    };
    let workspace = Workspace::new(root, config)?;
    for repo in workspace.repos() {
        if !repo.source.is_dir() {
            bail!("{} is not a folder", paths::display(&repo.source));
        }
    }
    let text = serde_json::to_string_pretty(&workspace.config)? + "\n";
    fs::write(&file, text).with_context(|| format!("writing {}", paths::display(&file)))?;
    Ok(file)
}

fn relative_to(root: &Path, p: &str) -> String {
    let root = paths::normalize(
        &std::path::absolute(root)
            .map_or_else(|_| root.to_string_lossy().into_owned(), |r| r.to_string_lossy().into_owned()),
    );
    let abs = paths::absolute(&root, p);
    match paths::rest_under(&abs, &root) {
        Some(rest) => rest.replace('\\', "/"),
        None => paths::forward(&abs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_overrides_and_resolves_paths() {
        let config: Config = serde_json::from_str(
            r#"{"repos": [{"path": "repos/cloneA"}],
                "overrides": {"repos/cloneA/a.h": {"as": "cp932", "judged": "ambiguous-cp932-euc-jp"},
                              "repos/cloneA/b.bin": {"as": "skip"}}}"#,
        )
        .unwrap();
        let ws = Workspace::new(Path::new(r"C:\ws"), config).unwrap();
        let repos = ws.repos();
        assert_eq!(repos[0].name, "cloneA");
        assert_eq!(repos[0].source, PathBuf::from(r"C:\ws\repos\cloneA"));
        assert_eq!(repos[0].mirror, PathBuf::from(r"C:\ws\mirror\cloneA"));
        let o = ws.override_for(Path::new(r"c:\WS\repos\cloneA\A.h")).unwrap();
        assert_eq!(o.read_as, ReadAs::Encoding(Encoding::Cp932));
        assert_eq!(ws.override_for(Path::new(r"C:\ws\repos\cloneA\b.bin")).unwrap().read_as, ReadAs::Skip);
        assert_eq!(ws.display_path(Path::new(r"C:\ws\repos\cloneA\a.h")), "repos/cloneA/a.h");
        assert_eq!(ws.display_path(Path::new(r"D:\lib\x.h")), "D:/lib/x.h");
    }

    #[test]
    fn rejects_a_judgement_as_an_override_and_a_mirror_inside_a_repo() {
        let bad = serde_json::from_str::<Config>(r#"{"overrides": {"a": {"as": "binary"}}}"#);
        assert!(bad.is_err());
        let config: Config = serde_json::from_str(r#"{"repos": [{"path": "."}]}"#).unwrap();
        assert!(Workspace::new(Path::new(r"C:\ws"), config).is_err());
    }
}
