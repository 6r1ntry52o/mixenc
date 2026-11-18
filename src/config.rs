//! A workspace's settings file: `mixenc.json`, or `<name>.mixenc.json` (several can share a folder,
//! such as a vault kept in git). Paths in it are relative to its folder or absolute. It says where
//! each repository is and where its mirror goes; the mirrors stay out of git:
//!
//! ```json
//! {
//!   "format_version": 1,
//!   "mirror": "D:/mirror",
//!   "repos": [{ "path": "C:/src/cloneA", "mirror": "D:/mirror/cloneA" }],
//!   "external": ["C:/Vendor/SDK/include"],
//!   "overrides": { "C:/src/cloneA/src/old.h": { "as": "cp932", "judged": "ambiguous-cp932-euc-jp" } }
//! }
//! ```
//!
//! `mirror` holds the mirrors of the include folders outside the repositories (`_ext`), mixenc's
//! state (`.mixenc`) and, unless a repository says otherwise, each repository's mirror
//! (`<mirror>/<name>`). Without it, the mirrors go to `mirror` beside the file, which is refused
//! when that is in a git work tree.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::judge::Encoding;
use crate::paths;

pub const FILE_NAME: &str = "mixenc.json";
/// The end of a named settings file's name (`cloneA.mixenc.json`).
pub const FILE_SUFFIX: &str = ".mixenc.json";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "format_version")]
    pub format_version: u32,
    /// Where the mirrors go (default: `mirror` beside this file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
    #[serde(default)]
    pub repos: Vec<Repo>,
    /// Include folders outside the repositories to mirror, besides those the databases name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external: Vec<String>,
    /// Encodings a person decided, by path: relative to the workspace with `/`, or absolute for a
    /// file outside it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, Override>,
    /// The clangd the entry starts (default: `MIXENC_CLANGD`, else `clangd.exe` on PATH, else the
    /// one vscode-clangd installed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clangd: Option<String>,
}

fn format_version() -> u32 {
    FORMAT_VERSION
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repo {
    /// The repository's folder, relative to the workspace or absolute.
    pub path: String,
    /// Its name (default: the repository folder's name), also the name of its mirror folder under
    /// `mirror`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its mirror (default: `<mirror>/<name>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
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
    /// The folder of the settings file: what its relative paths start from.
    pub root: PathBuf,
    /// The settings file.
    pub path: PathBuf,
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
    /// The workspace holding `start` (a file or folder of a repository or of its mirror): a
    /// settings file in a folder at or above it, else a registered one, whose repository or mirror
    /// holds it. Failing that, the nearest folder upward with one settings file (`start` is in the
    /// folder of the settings, such as `mirror/` beside them).
    pub fn find(start: &Path) -> Result<Workspace> {
        let start = std::path::absolute(start)?;
        let s = paths::canonical(&start).to_string_lossy().into_owned();
        let mut nearest: Option<Vec<Workspace>> = None;
        for dir in start.ancestors() {
            let files = config_files(dir);
            if files.is_empty() {
                continue;
            }
            let mut loaded = Vec::new();
            for file in &files {
                let ws = Workspace::load_file(file)?;
                if ws.holds(&s) {
                    return Ok(ws);
                }
                loaded.push(ws);
            }
            if nearest.is_none() {
                nearest = Some(loaded);
            }
        }
        for file in registered() {
            if let Ok(ws) = Workspace::load_file(&file)
                && ws.holds(&s)
            {
                return Ok(ws);
            }
        }
        match nearest {
            Some(mut loaded) if loaded.len() == 1 => Ok(loaded.remove(0)),
            Some(loaded) => bail!(
                "{} has {} settings files ({}); say which with --config",
                paths::display(&loaded[0].root),
                loaded.len(),
                loaded
                    .iter()
                    .filter_map(|w| w.path.file_name())
                    .map(|n| n.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None => bail!(
                "no mixenc.json or *.mixenc.json in {} or above it, and no registered workspace holds it (mixenc init writes one; mixenc register adds one)",
                paths::display(&start)
            ),
        }
    }

    /// Whether a repository, a mirror or the mirror folder holds the path (canonical).
    pub fn holds(&self, p: &str) -> bool {
        paths::is_under(p, &self.mirror_root().to_string_lossy())
            || self.repos().iter().any(|r| {
                paths::is_under(p, &r.source.to_string_lossy()) || paths::is_under(p, &r.mirror.to_string_lossy())
            })
    }

    /// The workspace of `mixenc.json` in `root`.
    pub fn load(root: &Path) -> Result<Workspace> {
        Workspace::load_file(&root.join(FILE_NAME))
    }

    /// The workspace of a settings file.
    pub fn load_file(file: &Path) -> Result<Workspace> {
        let file = paths::canonical(&std::path::absolute(file)?);
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
        let ws = Workspace::with_file(&file, config)?;
        ws.check_git()?;
        Ok(ws)
    }

    /// A workspace of `mixenc.json` in `root` (not read from disk; the mirrors are not checked
    /// against git).
    pub fn new(root: &Path, config: Config) -> Result<Workspace> {
        Workspace::with_file(&std::path::absolute(root)?.join(FILE_NAME), config)
    }

    fn with_file(file: &Path, config: Config) -> Result<Workspace> {
        let path = paths::canonical(&std::path::absolute(file)?);
        let root = path.parent().map(Path::to_path_buf).context("a settings file has a folder")?;
        let mut overrides = BTreeMap::new();
        for (p, o) in &config.overrides {
            let abs = paths::absolute(&root.to_string_lossy(), p);
            overrides.insert(paths::key(&abs), (p.clone(), o.clone()));
        }
        let workspace = Workspace { root, path, config, overrides };
        workspace.check_places()?;
        Ok(workspace)
    }

    /// The repositories' names are different, and no mirror is inside a repository or inside
    /// another mirror (a mirror removes what its original does not have).
    fn check_places(&self) -> Result<()> {
        let repos = self.repos();
        let mut names = BTreeMap::new();
        for repo in &repos {
            if let Some(other) = names.insert(repo.name.to_lowercase(), repo.source.clone()) {
                bail!(
                    "two repositories are named `{}`: {} and {} (give one a `name`)",
                    repo.name,
                    paths::display(&other),
                    paths::display(&repo.source)
                );
            }
        }
        let mirror_root = self.mirror_root();
        // The folders under the mirror folder that hold mixenc's own things.
        let own = [self.external_root(), self.state_dir()];
        for (i, repo) in repos.iter().enumerate() {
            for place in std::iter::once(&mirror_root).chain(repos.iter().map(|r| &r.mirror)) {
                if paths::is_under(&place.to_string_lossy(), &repo.source.to_string_lossy()) {
                    bail!(
                        "the mirror {} is inside the repository {}",
                        paths::display(place),
                        paths::display(&repo.source)
                    );
                }
            }
            let m = paths::display(&repo.mirror);
            if paths::is_under(&mirror_root.to_string_lossy(), &m) {
                bail!("the mirror of {} ({m}) holds the mirror folder {}", repo.name, paths::display(&mirror_root));
            }
            for o in &own {
                let o = o.to_string_lossy();
                if paths::is_under(&m, &o) || paths::is_under(&o, &m) {
                    bail!("the mirror of {} ({m}) overlaps mixenc's folder {o}", repo.name);
                }
            }
            for (j, other) in repos.iter().enumerate() {
                if i != j && paths::is_under(&m, &other.mirror.to_string_lossy()) {
                    bail!(
                        "the mirror of {} ({m}) is the mirror of {} or inside it; give each repository a folder of its own",
                        repo.name,
                        other.name
                    );
                }
            }
        }
        Ok(())
    }

    /// No mirror is in a git work tree: the settings file may be kept in git, the mirrors not.
    fn check_git(&self) -> Result<()> {
        for place in std::iter::once(self.mirror_root()).chain(self.repos().into_iter().map(|r| r.mirror)) {
            if let Some(top) = git_work_tree(&place) {
                let hint = if self.config.mirror.is_none() {
                    format!("write \"mirror\": \"<a folder outside git>\" in {}", paths::display(&self.path))
                } else {
                    format!("give it a folder outside git in {}", paths::display(&self.path))
                };
                bail!(
                    "the mirror {} would be in the git work tree {}: {hint}",
                    paths::display(&place),
                    paths::display(&top)
                );
            }
        }
        Ok(())
    }

    /// The settings file.
    pub fn file(&self) -> PathBuf {
        self.path.clone()
    }

    /// Where the mirrors go: `mirror` in the settings, else `mirror` beside them.
    pub fn mirror_root(&self) -> PathBuf {
        match &self.config.mirror {
            Some(m) => paths::canonical(&self.resolve(m)),
            None => self.root.join("mirror"),
        }
    }

    /// Where the mirrors of the include folders outside the repositories go.
    pub fn external_root(&self) -> PathBuf {
        self.mirror_root().join("_ext")
    }

    /// The workspace's own state (the toolchain scan, the entry's log).
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
                    mirror: match &r.mirror {
                        Some(m) => paths::canonical(&self.resolve(m)),
                        None => self.mirror_root().join(&name),
                    },
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

/// What `mixenc init` is given (paths relative to `dir`, the folder it runs in).
pub struct Init<'a> {
    pub dir: &'a Path,
    /// The settings file to write (default: `mixenc.json` in `dir`).
    pub file: Option<&'a Path>,
    pub repos: &'a [String],
    pub wiki: Option<&'a str>,
    /// Where the mirrors go (default: `mirror` beside the file).
    pub mirror: Option<&'a str>,
}

/// Writes a new settings file for the repositories, and registers it when a repository is outside
/// its folder (so that the repository finds it).
pub fn init(init: &Init) -> Result<PathBuf> {
    let dir = std::path::absolute(init.dir)?;
    let file = match init.file {
        Some(f) => std::path::absolute(dir.join(f))?,
        None => dir.join(FILE_NAME),
    };
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !is_config_name(&name) {
        bail!("{} is not a settings file's name: mixenc.json or <name>{FILE_SUFFIX}", paths::display(&file));
    }
    if file.exists() {
        bail!("{} exists already", paths::display(&file));
    }
    let root = file.parent().context("a settings file has a folder")?.to_path_buf();
    let from_dir = |p: &str| relative_to(&root, &paths::absolute(&dir.to_string_lossy(), p));
    let config = Config {
        format_version: FORMAT_VERSION,
        mirror: init.mirror.map(from_dir),
        repos: init
            .repos
            .iter()
            .enumerate()
            .map(|(i, path)| Repo {
                path: from_dir(path),
                name: None,
                mirror: None,
                wiki: if i == 0 { init.wiki.map(from_dir) } else { None },
                db: None,
            })
            .collect(),
        external: Vec::new(),
        overrides: BTreeMap::new(),
        clangd: None,
    };
    let workspace = Workspace::with_file(&file, config)?;
    workspace.check_git()?;
    for repo in workspace.repos() {
        if !repo.source.is_dir() {
            bail!("{} is not a folder", paths::display(&repo.source));
        }
    }
    fs::create_dir_all(&workspace.root).with_context(|| format!("creating {}", paths::display(&workspace.root)))?;
    let text = serde_json::to_string_pretty(&workspace.config)? + "\n";
    fs::write(&workspace.path, text).with_context(|| format!("writing {}", paths::display(&workspace.path)))?;
    if workspace.needs_register() {
        register(&workspace.path)?;
    }
    Ok(workspace.path)
}

impl Workspace {
    /// Whether a repository is outside the settings file's folder, so that only the register leads
    /// from it to the file.
    pub fn needs_register(&self) -> bool {
        self.repos().iter().any(|r| !paths::is_under(&r.source.to_string_lossy(), &self.root.to_string_lossy()))
    }
}

/// Why two workspaces cannot be served together: a mirror of one inside a mirror of the other
/// (the same repository with the same mirror is shared, not an overlap), or the same repository
/// with two mirrors.
pub fn overlap(a: &Workspace, b: &Workspace) -> Option<String> {
    let places = |w: &Workspace| {
        let mut v: Vec<(Option<PathBuf>, PathBuf)> =
            w.repos().into_iter().map(|r| (Some(r.source), r.mirror)).collect();
        v.push((None, w.external_root()));
        v
    };
    for (sa, ma) in places(a) {
        for (sb, mb) in places(b) {
            let same_source = match (&sa, &sb) {
                (Some(x), Some(y)) => paths::key(&x.to_string_lossy()) == paths::key(&y.to_string_lossy()),
                (None, None) => true,
                _ => false,
            };
            let (ka, kb) = (paths::key(&ma.to_string_lossy()), paths::key(&mb.to_string_lossy()));
            if same_source {
                if sa.is_some() && ka != kb {
                    return Some(format!(
                        "{} is mirrored to {} there and to {} here",
                        paths::display(sa.as_deref().unwrap_or(Path::new(""))),
                        paths::display(&mb),
                        paths::display(&ma)
                    ));
                }
                continue;
            }
            if paths::is_under(&ka, &kb) || paths::is_under(&kb, &ka) {
                return Some(format!(
                    "its mirror {} and the mirror {} of {} overlap",
                    paths::display(&mb),
                    paths::display(&ma),
                    paths::display(&a.path)
                ));
            }
        }
    }
    None
}

/// Whether a file name is a settings file's: `mixenc.json` or `<name>.mixenc.json`.
pub fn is_config_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == FILE_NAME || (lower.ends_with(FILE_SUFFIX) && lower.len() > FILE_SUFFIX.len())
}

/// The settings files in a folder: `mixenc.json` first, then the named ones by name.
pub fn config_files(dir: &Path) -> Vec<PathBuf> {
    let mut named: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            name != FILE_NAME && is_config_name(&name)
        })
        .collect();
    named.sort();
    let plain = dir.join(FILE_NAME);
    std::iter::once(plain).filter(|p| p.is_file()).chain(named).collect()
}

/// The top of the git work tree that holds `p` (a `.git` folder or file in it or above it).
pub fn git_work_tree(p: &Path) -> Option<PathBuf> {
    p.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

/// mixenc's folder for this user: `MIXENC_HOME`, else `%LOCALAPPDATA%\mixenc`.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("MIXENC_HOME") {
        return PathBuf::from(h);
    }
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("mixenc")
}

#[derive(Default, Serialize, Deserialize)]
struct Register {
    /// Settings files (a folder, as earlier versions wrote, stands for its `mixenc.json`).
    workspaces: Vec<String>,
}

fn register_file() -> PathBuf {
    home().join("workspaces.json")
}

fn registered_file(entry: &str) -> PathBuf {
    let p = PathBuf::from(entry);
    if p.is_dir() { p.join(FILE_NAME) } else { p }
}

/// The registered settings files that are still there.
pub fn registered() -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string(register_file()) else { return Vec::new() };
    let r: Register = serde_json::from_str(&text).unwrap_or_default();
    r.workspaces.iter().map(|w| registered_file(w)).filter(|p| p.is_file()).collect()
}

/// Adds a settings file to the register (and drops the ones that are gone). `Ok(false)` when it
/// was there already.
pub fn register(file: &Path) -> Result<bool> {
    let reg = register_file();
    let mut r: Register = fs::read_to_string(&reg).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let shown = paths::display(&paths::canonical(&std::path::absolute(file)?));
    let before = r.workspaces.len();
    r.workspaces.retain(|w| registered_file(w).is_file());
    let present = r.workspaces.iter().any(|w| paths::key(&registered_file(w).to_string_lossy()) == paths::key(&shown));
    if present && r.workspaces.len() == before {
        return Ok(false);
    }
    if !present {
        r.workspaces.push(shown);
    }
    let dir = home();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", paths::display(&dir)))?;
    crate::fsx::write_replace(&dir, &reg, (serde_json::to_string_pretty(&r)? + "\n").as_bytes(), false)
        .with_context(|| format!("writing {}", paths::display(&reg)))?;
    Ok(!present)
}

/// Writes (or with `None` removes) a file's entry in the override table of `mixenc.json`, with the
/// file's judgement now, so that the doctor sees when it changes. The rest of the file is kept.
pub fn set_override(ws: &Workspace, abs: &Path, read_as: Option<ReadAs>, judged: Option<Encoding>) -> Result<bool> {
    let file = ws.file();
    let text = fs::read_to_string(&file).with_context(|| format!("reading {}", paths::display(&file)))?;
    let mut config: Config =
        serde_json::from_str(&text).with_context(|| format!("reading {}", paths::display(&file)))?;
    let key = ws.display_path(abs);
    // An entry written another way (absolute, other case) for the same file is replaced too.
    let same: Vec<String> = config
        .overrides
        .keys()
        .filter(|k| paths::key(&paths::absolute(&ws.root.to_string_lossy(), k)) == paths::key(&abs.to_string_lossy()))
        .cloned()
        .collect();
    let mut changed = false;
    for k in same {
        changed |= config.overrides.remove(&k).is_some();
    }
    if let Some(read_as) = read_as {
        config.overrides.insert(key, Override { read_as, judged });
        changed = true;
    }
    let text = serde_json::to_string_pretty(&config)? + "\n";
    crate::fsx::write_replace(&ws.root, &file, text.as_bytes(), false)
        .with_context(|| format!("writing {}", paths::display(&file)))?;
    Ok(changed)
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

    fn ws(json: &str) -> Result<Workspace> {
        Workspace::new(Path::new(r"C:\ws"), serde_json::from_str(json).unwrap())
    }

    #[test]
    fn mirrors_go_where_the_settings_say() {
        let w = ws(r#"{"mirror": "D:/m", "repos": [{"path": "C:/src/a"}, {"path": "C:/src/b", "mirror": "E:/b"}]}"#)
            .unwrap();
        let repos = w.repos();
        assert_eq!(repos[0].mirror, PathBuf::from(r"D:\m\a"));
        assert_eq!(repos[1].mirror, PathBuf::from(r"E:\b"));
        assert_eq!(w.external_root(), PathBuf::from(r"D:\m\_ext"));
        assert!(w.holds(r"D:\m\a\x.cpp") && w.holds(r"C:\src\b\y.h") && !w.holds(r"C:\src\c"));
        // A mirror inside another, or over mixenc's own folders, is refused.
        assert!(
            ws(r#"{"mirror": "D:/m", "repos": [{"path": "C:/a"}, {"path": "C:/b", "mirror": "D:/m/a/b"}]}"#).is_err()
        );
        assert!(ws(r#"{"mirror": "D:/m", "repos": [{"path": "C:/a", "mirror": "D:/m/_ext"}]}"#).is_err());
        assert!(ws(r#"{"mirror": "D:/m/a/inner", "repos": [{"path": "C:/a", "mirror": "D:/m/a"}]}"#).is_err());
        assert!(ws(r#"{"mirror": "C:/a/m", "repos": [{"path": "C:/a"}]}"#).is_err());
    }

    #[test]
    fn two_workspaces_share_a_mirror_folder_but_not_a_mirror() {
        let a = ws(r#"{"mirror": "D:/m", "repos": [{"path": "C:/src/a"}]}"#).unwrap();
        let b = ws(r#"{"mirror": "D:/m", "repos": [{"path": "C:/src/b"}]}"#).unwrap();
        assert_eq!(overlap(&a, &b), None);
        let same = ws(r#"{"mirror": "D:/m", "repos": [{"path": "C:/src/a"}]}"#).unwrap();
        assert_eq!(overlap(&a, &same), None, "the same repository with the same mirror is shared");
        let elsewhere = ws(r#"{"mirror": "D:/n", "repos": [{"path": "C:/src/a"}]}"#).unwrap();
        assert!(overlap(&a, &elsewhere).is_some(), "one repository, two mirrors");
        let inside = ws(r#"{"mirror": "D:/n", "repos": [{"path": "C:/src/c", "mirror": "D:/m/a/c"}]}"#).unwrap();
        assert!(overlap(&a, &inside).is_some());
    }

    #[test]
    fn settings_files_are_found_by_name() {
        assert!(
            is_config_name("mixenc.json") && is_config_name("cloneA.mixenc.json") && is_config_name("A.MIXENC.JSON")
        );
        assert!(!is_config_name(".mixenc.json") && !is_config_name("mixenc.jsonc") && !is_config_name("x.json"));
        let tmp = tempfile::tempdir().unwrap();
        let dir = paths::canonical(tmp.path());
        for (name, repo) in [("a.mixenc.json", "ra"), ("b.mixenc.json", "rb")] {
            fs::create_dir_all(dir.join(repo)).unwrap();
            fs::write(dir.join(name), format!(r#"{{"repos": [{{"path": "{repo}"}}], "mirror": "m-{repo}"}}"#)).unwrap();
        }
        assert_eq!(config_files(&dir).len(), 2);
        // The one whose repository holds the folder; in the folder itself, neither is right.
        assert!(Workspace::find(&dir.join("rb")).unwrap().path.ends_with("b.mixenc.json"));
        assert!(Workspace::find(&dir.join("m-ra").join("x")).unwrap().path.ends_with("a.mixenc.json"));
        assert!(Workspace::find(&dir).is_err());
    }

    #[test]
    fn rejects_a_judgement_as_an_override_and_a_mirror_inside_a_repo() {
        let bad = serde_json::from_str::<Config>(r#"{"overrides": {"a": {"as": "binary"}}}"#);
        assert!(bad.is_err());
        let config: Config = serde_json::from_str(r#"{"repos": [{"path": "."}]}"#).unwrap();
        assert!(Workspace::new(Path::new(r"C:\ws"), config).is_err());
    }
}
