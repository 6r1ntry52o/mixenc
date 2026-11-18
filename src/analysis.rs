//! The analysis tools of `mixenc mcp`: clangd's answers made short for an agent, every path the
//! original's. A question about a place takes the file, the line and the symbol's name on it (no
//! columns to count), or only the symbol's name, which the index resolves.
//!
//! Before it answers, a tool checks what it can about the file asked about (mirrored, read right,
//! in the database, the index complete) and adds what it could not trust as notes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::config::Workspace;
use crate::judge::Encoding;
use crate::locate::{self, Place};
use crate::lspclient::{Client, IndexState};
use crate::search::{self, Matcher, Options};
use crate::{db, doctor, paths, uri};

/// How long a question about the index waits for clangd's background index to finish.
const INDEX_WAIT: Duration = Duration::from_secs(20);
const ASK: Duration = Duration::from_secs(90);
const READ_LIMIT: usize = 400;
const TEXT_LIMIT: usize = 200;
const TREE_LIMIT: usize = 150;

/// The tools' state: one clangd per workspace, started by the first question that needs it.
#[derive(Default)]
pub struct Tools {
    analyzers: HashMap<String, Analyzer>,
}

struct Analyzer {
    ws: Workspace,
    client: Option<Client>,
    /// The sources the databases name (path keys of the originals), with the databases' times.
    db_files: Option<(Vec<Option<SystemTime>>, HashSet<String>)>,
}

/// A place in a file: 0-based line, UTF-16 column (LSP's).
#[derive(Clone, Debug)]
struct At {
    path: PathBuf,
    line: usize,
    character: usize,
    /// The symbol's name as asked.
    symbol: String,
}

impl Tools {
    /// Tools that know these workspaces from the start (`mixenc mcp --config ...`: a window with
    /// several settings files), besides the ones found from paths and the current folder.
    pub fn with(workspaces: Vec<Workspace>) -> Tools {
        let mut tools = Tools::default();
        for ws in workspaces {
            tools
                .analyzers
                .insert(paths::key(&ws.path.to_string_lossy()), Analyzer { ws, client: None, db_files: None });
        }
        tools
    }

    /// Answers one tool call; `Err` says what to change in the call.
    pub fn call(&mut self, name: &str, args: &Value) -> Result<String> {
        let cwd = std::env::current_dir().unwrap_or_default();
        let path = args.get("path").and_then(Value::as_str).filter(|p| !p.is_empty()).map(|p| self.resolve(p, &cwd));
        let ws = self.workspace_for(path.as_deref(), &cwd)?;
        let key = paths::key(&ws.path.to_string_lossy());
        let analyzer = self.analyzers.entry(key).or_insert_with(|| Analyzer { ws, client: None, db_files: None });
        let out = Out { cwd: &cwd };
        match name {
            "find_symbol" => analyzer.find_symbol(args, &out),
            "definition" => analyzer.definition(args, path, &out),
            "references" => analyzer.references(args, path, &out),
            "call_hierarchy" => analyzer.call_hierarchy(args, path, &out),
            "type_hierarchy" => analyzer.type_hierarchy(args, path, &out),
            "outline" => analyzer.outline(path, &out),
            "read_lines" => analyzer.read_lines(args, path, &out),
            "search" => analyzer.search(args, path, &out),
            "diagnose" => analyzer.diagnose(path, &out),
            _ => Err(anyhow!("unknown tool {name}")),
        }
    }

    /// A path as given: absolute, or relative to the current folder or to a workspace in use.
    fn resolve(&self, p: &str, cwd: &Path) -> PathBuf {
        let p = match p.strip_prefix("file:") {
            Some(_) => uri::to_path(p).unwrap_or_else(|| PathBuf::from(p)),
            None => PathBuf::from(p),
        };
        if p.is_absolute() {
            return p;
        }
        let candidates = std::iter::once(cwd.to_path_buf()).chain(
            self.analyzers
                .values()
                .flat_map(|a| std::iter::once(a.ws.root.clone()).chain(a.ws.repos().into_iter().map(|r| r.source))),
        );
        for base in candidates {
            let joined = base.join(&p);
            if joined.exists() {
                return joined;
            }
        }
        cwd.join(p)
    }

    fn workspace_for(&self, path: Option<&Path>, cwd: &Path) -> Result<Workspace> {
        if let Some(p) = path {
            if let Some(place) = locate::find(p, None) {
                return Ok(place.ws);
            }
            for a in self.analyzers.values() {
                if locate::find(p, Some(&a.ws)).is_some() {
                    return Ok(a.ws.clone());
                }
            }
            if let Ok(ws) = Workspace::find(p) {
                return Ok(ws);
            }
        }
        if let Ok(ws) = Workspace::find(cwd) {
            return Ok(ws);
        }
        match self.analyzers.values().collect::<Vec<_>>().as_slice() {
            [one] => Ok(one.ws.clone()),
            _ => {
                bail!("no mixenc workspace here ({}); give path: a file or folder of a repository", paths::display(cwd))
            }
        }
    }
}

/// How answers name files: relative to the current folder when under it, else absolute; `/`.
struct Out<'a> {
    cwd: &'a Path,
}

impl Out<'_> {
    fn path(&self, abs: &Path) -> String {
        let s = abs.to_string_lossy();
        match paths::rest_under(&s, &self.cwd.to_string_lossy()) {
            Some(rest) if !rest.is_empty() => rest.replace('\\', "/"),
            _ => paths::forward(&s),
        }
    }

    fn at(&self, abs: &Path, line0: usize) -> String {
        format!("{}:{}", self.path(abs), line0 + 1)
    }
}

impl Analyzer {
    fn client(&mut self) -> Result<&Client> {
        if self.client.as_ref().is_some_and(|c| !c.is_alive()) {
            self.client = None;
        }
        if self.client.is_none() {
            let folders: Vec<PathBuf> = self.ws.repos().into_iter().map(|r| r.source).collect();
            let client = Client::start(&folders)?;
            // clangd finds a repository's database when it opens one of its files; open one per
            // repository, so that the background index starts.
            for repo in self.ws.repos() {
                let first = db::read(&repo.mirror.join("compile_commands.json"))
                    .ok()
                    .and_then(|entries| entries.into_iter().next())
                    .map(|e| PathBuf::from(paths::absolute(&e.directory, &e.file)));
                if let Some(original) = first.and_then(|m| locate::find(&m, Some(&self.ws))).map(|p| p.original())
                    && let Some(lines) = locate::lines(&original, Some(&self.ws))
                {
                    let _ = client.open(&original, &(lines.join("\n") + "\n"));
                }
            }
            self.client = Some(client);
        }
        Ok(self.client.as_ref().unwrap())
    }

    /// Opens a file in clangd with its text read right; returns its URI and its lines.
    fn open(&mut self, original: &Path) -> Result<(String, Vec<String>)> {
        let lines = locate::lines(original, Some(&self.ws))
            .with_context(|| format!("{} cannot be read", paths::display(original)))?;
        let text = lines.join("\n") + "\n";
        let uri = self.client()?.open(original, &text)?;
        Ok((uri, lines))
    }

    fn place(&self, path: &Path) -> Option<Place> {
        locate::find(path, Some(&self.ws)).or_else(|| locate::find(path, None))
    }

    /// The original a path stands for (a mirror path stands for its original).
    fn original(&self, path: &Path) -> PathBuf {
        match self.place(path) {
            Some(place) if place.in_mirror => place.original(),
            _ => paths::canonical(path),
        }
    }

    /// What the answer about this file cannot vouch for.
    fn file_notes(&mut self, original: &Path) -> Vec<String> {
        let mut notes = Vec::new();
        let Some(place) = self.place(original) else {
            notes
                .push("this file is outside the workspace's mirrored folders: clangd reads it as it is on disk".into());
            return notes;
        };
        match place.state() {
            None => notes.push("this file has no mirror yet (mixenc sync)".into()),
            Some(state) => {
                if let Some(problem) = &state.problem {
                    notes.push(format!(
                        "this file is not mirrored ({}): clangd may read it wrong (mixenc doctor)",
                        problem_text(problem)
                    ));
                }
                if matches!(state.judged, Encoding::Mixed | Encoding::Broken) {
                    notes.push(format!(
                        "this file is {}: bytes that read as no character show as \u{FFFD}",
                        state.judged
                    ));
                }
            }
        }
        if is_source(original) && !self.in_db(original) {
            notes.push(
                "this file is not in compile_commands.json: clangd guessed how it is compiled (mixenc doctor)".into(),
            );
        }
        notes
    }

    fn in_db(&mut self, original: &Path) -> bool {
        let dbs: Vec<PathBuf> = self.ws.repos().iter().map(|r| r.mirror.join("compile_commands.json")).collect();
        let times: Vec<Option<SystemTime>> =
            dbs.iter().map(|d| std::fs::metadata(d).and_then(|m| m.modified()).ok()).collect();
        if self.db_files.as_ref().is_none_or(|(t, _)| *t != times) {
            let mut files = HashSet::new();
            for d in &dbs {
                for e in db::read(d).unwrap_or_default() {
                    let mirror = PathBuf::from(paths::absolute(&e.directory, &e.file));
                    let original = locate::find(&mirror, Some(&self.ws)).map_or(mirror, |p| p.original());
                    files.insert(paths::key(&original.to_string_lossy()));
                }
            }
            self.db_files = Some((times, files));
        }
        self.db_files.as_ref().is_some_and(|(_, f)| f.contains(&paths::key(&original.to_string_lossy())))
    }

    /// The place a question is about: `path` + `line` + `symbol`, or `symbol` alone (from the index).
    fn at(&mut self, args: &Value, path: Option<PathBuf>, out: &Out) -> Result<(At, Vec<String>)> {
        let symbol = args["symbol"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let line = args["line"].as_u64().map(|l| l as usize);
        let mut notes = Vec::new();
        let (path, line, symbol) = match (path, line, symbol) {
            (Some(path), Some(line), Some(symbol)) => (self.original(&path), line, symbol.to_string()),
            // A file without a line: the first line where the name is written.
            (Some(path), None, Some(symbol)) => {
                let path = self.original(&path);
                let (_, lines) = self.open(&path)?;
                let line = (1..=lines.len())
                    .find(|&l| find_name(&lines[l - 1..l], 1, symbol).is_ok())
                    .with_context(|| format!("{symbol} is not written in {}", out.path(&path)))?;
                (path, line, symbol.to_string())
            }
            (_, _, Some(symbol)) => {
                let (path, line) = self.resolve_symbol(symbol, out, &mut notes)?;
                (path, line + 1, symbol.to_string())
            }
            _ => bail!("give symbol (a name, Foo::Init), or path, line and the symbol's name on that line"),
        };
        let (_, lines) = self.open(&path)?;
        let (line0, character) =
            find_name(&lines, line, &symbol).map_err(|e| anyhow!("{}: {e}", out.at(&path, line - 1)))?;
        if line0 + 1 != line {
            notes.push(format!("{symbol} is on line {} (not {line})", line0 + 1));
        }
        notes.extend(self.file_notes(&path));
        Ok((At { path, line: line0, character, symbol }, notes))
    }

    /// Where a symbol named `symbol` is, from clangd's index; several candidates are an error that
    /// lists them.
    fn resolve_symbol(&mut self, symbol: &str, out: &Out, notes: &mut Vec<String>) -> Result<(PathBuf, usize)> {
        let found = self.workspace_symbols(symbol, notes)?;
        let wanted = symbol.trim_start_matches("::");
        let exact: Vec<&Sym> = found.iter().filter(|s| s.qualified() == wanted || s.name == wanted).collect();
        let tail: Vec<&Sym> = found.iter().filter(|s| s.qualified().ends_with(&format!("::{wanted}"))).collect();
        let chosen = if exact.is_empty() { tail } else { exact };
        match chosen.as_slice() {
            [] => bail!(
                "no symbol {symbol} in clangd's index{}; find_symbol searches by part of a name",
                notes.last().map(|n| format!(" ({n})")).unwrap_or_default()
            ),
            [one] => Ok((one.path.clone(), one.line)),
            many => {
                let list: Vec<String> = many
                    .iter()
                    .take(10)
                    .map(|s| format!("{} {}  {}", s.kind, s.qualified(), out.at(&s.path, s.line)))
                    .collect();
                bail!(
                    "{symbol} names {} symbols; call again with path and line of one:\n{}",
                    many.len(),
                    list.join("\n")
                )
            }
        }
    }

    fn workspace_symbols(&mut self, query: &str, notes: &mut Vec<String>) -> Result<Vec<Sym>> {
        let client = self.client()?;
        let state = client.wait_index(INDEX_WAIT);
        notes.extend(state.note());
        let result = client.request("workspace/symbol", json!({ "query": query }), ASK)?;
        let mut syms: Vec<Sym> = result.as_array().into_iter().flatten().filter_map(Sym::from_lsp).collect();
        for s in &mut syms {
            s.path = self.original(&s.path);
        }
        Ok(syms)
    }

    fn find_symbol(&mut self, args: &Value, out: &Out) -> Result<String> {
        let query =
            args["query"].as_str().filter(|q| !q.trim().is_empty()).context("give query: a name or part of one")?;
        let limit = args["limit"].as_u64().map_or(30, |l| l.clamp(1, 200) as usize);
        let mut notes = Vec::new();
        let mut syms = self.workspace_symbols(query.trim(), &mut notes)?;
        let q = query.trim().trim_start_matches("::");
        syms.sort_by_key(|s| (s.qualified() != q && s.name != q, s.qualified()));
        let mut text = format!("{} symbols for {query:?}", syms.len());
        if syms.len() > limit {
            text.push_str(&format!(" ({limit} shown)"));
        }
        text.push('\n');
        for s in syms.iter().take(limit) {
            text.push_str(&format!("{} {}  {}\n", s.kind, s.qualified(), out.at(&s.path, s.line)));
        }
        Ok(finish(text, notes))
    }

    fn definition(&mut self, args: &Value, path: Option<PathBuf>, out: &Out) -> Result<String> {
        let (at, notes) = self.at(args, path, out)?;
        let uri = self.client_uri(&at.path)?;
        let params =
            json!({ "textDocument": { "uri": uri }, "position": { "line": at.line, "character": at.character } });
        let client = self.client()?;
        // clangd's symbolInfo names the definition and the declaration apart; go-to-definition on a
        // definition answers the declaration instead.
        let info = client.request("textDocument/symbolInfo", params.clone(), ASK).ok();
        let info = info.as_ref().and_then(Value::as_array).and_then(|a| a.first());
        let range = |key: &str| info.map(|i| locations(&i[key])).unwrap_or_default();
        let (mut definitions, mut declarations) = (range("definitionRange"), range("declarationRange"));
        if definitions.is_empty() {
            definitions = locations(&client.request("textDocument/definition", params.clone(), ASK)?);
            definitions.retain(|d| !declarations.contains(d));
        }
        if declarations.is_empty() {
            declarations = locations(&client.request("textDocument/declaration", params, ASK)?);
        }
        let mut text = String::new();
        if definitions.is_empty() && declarations.is_empty() {
            text.push_str(&format!("clangd knows no definition of {} at {}\n", at.symbol, out.at(&at.path, at.line)));
        }
        let mut shown = HashSet::new();
        for (what, list, around) in [("definition", &definitions, 2), ("declaration", &declarations, 1)] {
            for (path, line) in list {
                let path = self.original(path);
                if !shown.insert((paths::key(&path.to_string_lossy()), *line)) {
                    continue;
                }
                text.push_str(&format!("{what} of {}: {}\n", at.symbol, out.at(&path, *line)));
                text.push_str(&self.excerpt(&path, *line, around));
            }
        }
        Ok(finish(text, notes))
    }

    fn client_uri(&mut self, original: &Path) -> Result<String> {
        Ok(self.open(original)?.0)
    }

    /// Lines around a line of a file, numbered, the line itself marked.
    fn excerpt(&self, path: &Path, line0: usize, around: usize) -> String {
        let Some(lines) = locate::lines(path, Some(&self.ws)) else { return String::new() };
        let first = line0.saturating_sub(around);
        let last = (line0 + around).min(lines.len().saturating_sub(1));
        let mut text = String::new();
        for (i, l) in lines.iter().enumerate().take(last + 1).skip(first) {
            let mark = if i == line0 { '>' } else { ' ' };
            text.push_str(&format!("{mark}{:>5}  {}\n", i + 1, clip(l)));
        }
        text
    }

    fn references(&mut self, args: &Value, path: Option<PathBuf>, out: &Out) -> Result<String> {
        let (at, mut notes) = self.at(args, path, out)?;
        let declaration = args["include_declaration"].as_bool().unwrap_or(false);
        let uri = self.client_uri(&at.path)?;
        let client = self.client()?;
        notes.extend(client.wait_index(INDEX_WAIT).note());
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": at.line, "character": at.character },
            "context": { "includeDeclaration": declaration },
        });
        let found = locations(&client.request("textDocument/references", params, ASK)?);
        let mut by_file: Vec<(PathBuf, Vec<usize>)> = Vec::new();
        for (path, line) in found {
            let path = self.original(&path);
            match by_file
                .iter_mut()
                .find(|(p, _)| paths::key(&p.to_string_lossy()) == paths::key(&path.to_string_lossy()))
            {
                Some((_, lines)) => lines.push(line + 1),
                None => by_file.push((path, vec![line + 1])),
            }
        }
        let total: usize = by_file.iter().map(|(_, l)| l.len()).sum();
        let mut text = format!(
            "{total} references to {} in {} files{}\n",
            at.symbol,
            by_file.len(),
            if declaration { " (declarations included)" } else { "" }
        );
        by_file.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, mut lines) in by_file.into_iter().take(60) {
            lines.sort_unstable();
            lines.dedup();
            let shown: Vec<String> = lines.iter().take(30).map(usize::to_string).collect();
            let more = if lines.len() > 30 { format!(" and {} more", lines.len() - 30) } else { String::new() };
            text.push_str(&format!("{}: {}{more}\n", out.path(&path), shown.join(", ")));
        }
        Ok(finish(text, notes))
    }

    fn call_hierarchy(&mut self, args: &Value, path: Option<PathBuf>, out: &Out) -> Result<String> {
        let incoming = match args["direction"].as_str().unwrap_or("incoming") {
            "incoming" | "callers" => true,
            "outgoing" | "callees" => false,
            other => bail!("direction is incoming (who calls it) or outgoing (what it calls), not {other:?}"),
        };
        let depth = args["depth"].as_u64().map_or(2, |d| d.clamp(1, 5) as usize);
        let (at, mut notes) = self.at(args, path, out)?;
        let uri = self.client_uri(&at.path)?;
        let client = self.client()?;
        if incoming {
            notes.extend(client.wait_index(INDEX_WAIT).note());
        }
        let params =
            json!({ "textDocument": { "uri": uri }, "position": { "line": at.line, "character": at.character } });
        let items = client.request("textDocument/prepareCallHierarchy", params, ASK)?;
        let Some(items) = items.as_array().filter(|a| !a.is_empty()) else {
            bail!(
                "{} at {} is not a function clangd can follow (call_hierarchy is for functions)",
                at.symbol,
                out.at(&at.path, at.line)
            );
        };
        let method = if incoming { "callHierarchy/incomingCalls" } else { "callHierarchy/outgoingCalls" };
        let arrow = if incoming { "<-" } else { "->" };
        let mut text = format!(
            "{} of {} ({} levels; {})\n",
            if incoming { "callers" } else { "callees" },
            at.symbol,
            depth,
            if incoming {
                "<- is called by, with the lines of the calls"
            } else {
                "-> calls, with the lines of the calls"
            }
        );
        let mut nodes = 0;
        let mut seen = HashSet::new();
        for item in items {
            self.walk_calls(item, method, arrow, incoming, depth, 0, &mut seen, &mut nodes, &mut text, out)?;
        }
        if nodes >= TREE_LIMIT {
            notes.push(format!(
                "the tree was cut at {TREE_LIMIT} entries; ask about a deeper entry, or with less depth"
            ));
        }
        Ok(finish(text, notes))
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_calls(
        &mut self,
        item: &Value,
        method: &str,
        arrow: &str,
        incoming: bool,
        depth: usize,
        level: usize,
        seen: &mut HashSet<String>,
        nodes: &mut usize,
        text: &mut String,
        out: &Out,
    ) -> Result<()> {
        let id = item_id(item);
        if level == 0 {
            text.push_str(&format!("{}  {}\n", item_label(item), self.item_at(item, out)));
        }
        if level >= depth || !seen.insert(id) {
            return Ok(());
        }
        let calls = self.client()?.request(method, json!({ "item": item }), ASK)?;
        for call in calls.as_array().into_iter().flatten() {
            if *nodes >= TREE_LIMIT {
                return Ok(());
            }
            *nodes += 1;
            let other = if incoming { &call["from"] } else { &call["to"] };
            let mut lines: Vec<usize> = call["fromRanges"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["start"]["line"].as_u64().map(|l| l as usize + 1))
                .collect();
            lines.sort_unstable();
            lines.dedup();
            let lines: Vec<String> = lines.iter().map(usize::to_string).collect();
            let repeat = if seen.contains(&item_id(other)) { "  (above)" } else { "" };
            text.push_str(&format!(
                "{}{arrow} {}  {}  (calls at {}){repeat}\n",
                "  ".repeat(level + 1),
                item_label(other),
                self.item_at(other, out),
                lines.join(", ")
            ));
            if repeat.is_empty() {
                self.walk_calls(other, method, arrow, incoming, depth, level + 1, seen, nodes, text, out)?;
            }
        }
        Ok(())
    }

    fn item_at(&self, item: &Value, out: &Out) -> String {
        let path = item["uri"].as_str().and_then(uri::to_path).map(|p| self.original(&p));
        let line = item["selectionRange"]["start"]["line"].as_u64().unwrap_or(0) as usize;
        path.map_or_else(|| "?".into(), |p| out.at(&p, line))
    }

    fn type_hierarchy(&mut self, args: &Value, path: Option<PathBuf>, out: &Out) -> Result<String> {
        let (up, down) = match args["direction"].as_str().unwrap_or("both") {
            "both" => (true, true),
            "supertypes" | "bases" | "up" => (true, false),
            "subtypes" | "derived" | "down" => (false, true),
            other => bail!("direction is supertypes, subtypes or both, not {other:?}"),
        };
        let depth = args["depth"].as_u64().map_or(3, |d| d.clamp(1, 6) as usize);
        let (at, mut notes) = self.at(args, path, out)?;
        let uri = self.client_uri(&at.path)?;
        let client = self.client()?;
        if down {
            notes.extend(client.wait_index(INDEX_WAIT).note());
        }
        let params =
            json!({ "textDocument": { "uri": uri }, "position": { "line": at.line, "character": at.character } });
        let items = client.request("textDocument/prepareTypeHierarchy", params, ASK)?;
        let Some(items) = items.as_array().filter(|a| !a.is_empty()).cloned() else {
            bail!(
                "{} at {} is not a class clangd can follow (type_hierarchy is for classes and structs)",
                at.symbol,
                out.at(&at.path, at.line)
            );
        };
        let mut text = String::new();
        let mut nodes = 0;
        for item in &items {
            text.push_str(&format!("{}  {}\n", item_label(item), self.item_at(item, out)));
            for (wanted, method, title) in
                [(up, "typeHierarchy/supertypes", "bases"), (down, "typeHierarchy/subtypes", "derived")]
            {
                if !wanted {
                    continue;
                }
                text.push_str(&format!("  {title}:\n"));
                let before = text.len();
                self.walk_types(item, method, depth, 2, &mut HashSet::new(), &mut nodes, &mut text, out)?;
                if text.len() == before {
                    text.push_str("    (none)\n");
                }
            }
        }
        Ok(finish(text, notes))
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_types(
        &mut self,
        item: &Value,
        method: &str,
        depth: usize,
        indent: usize,
        seen: &mut HashSet<String>,
        nodes: &mut usize,
        text: &mut String,
        out: &Out,
    ) -> Result<()> {
        if depth == 0 || !seen.insert(item_id(item)) {
            return Ok(());
        }
        let found = self.client()?.request(method, json!({ "item": item }), ASK)?;
        for other in found.as_array().into_iter().flatten() {
            if *nodes >= TREE_LIMIT {
                return Ok(());
            }
            *nodes += 1;
            text.push_str(&format!("{}{}  {}\n", "  ".repeat(indent), item_label(other), self.item_at(other, out)));
            self.walk_types(other, method, depth - 1, indent + 1, seen, nodes, text, out)?;
        }
        Ok(())
    }

    fn outline(&mut self, path: Option<PathBuf>, out: &Out) -> Result<String> {
        let path = self.original(&path.context("give path: the file")?);
        let notes = self.file_notes(&path);
        let (uri, lines) = self.open(&path)?;
        let symbols =
            self.client()?.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": uri } }), ASK)?;
        let mut text = format!("{} ({} lines)\n", out.path(&path), lines.len());
        let mut count = 0;
        outline_lines(symbols.as_array().map_or(&[][..], Vec::as_slice), 0, &mut count, &mut text);
        if count >= 300 {
            text.push_str("... (cut at 300 entries)\n");
        }
        Ok(finish(text, notes))
    }

    fn read_lines(&mut self, args: &Value, path: Option<PathBuf>, out: &Out) -> Result<String> {
        let path = self.original(&path.context("give path: the file")?);
        let lines =
            locate::lines(&path, Some(&self.ws)).with_context(|| format!("{} cannot be read", out.path(&path)))?;
        let start = args["start"].as_u64().map_or(1, |s| s.max(1) as usize);
        let end = args["end"].as_u64().map_or(start + 99, |e| e as usize).min(lines.len()).min(start + READ_LIMIT - 1);
        let encoding = self.place(&path).and_then(|p| p.state()).map(|s| s.read_as.unwrap_or(s.judged).to_string());
        let mut text = format!(
            "{} lines {start}-{end} of {}{}\n",
            out.path(&path),
            lines.len(),
            encoding.map(|e| format!(" ({e}, read right)")).unwrap_or_default()
        );
        for (i, l) in lines.iter().enumerate().take(end).skip(start - 1) {
            text.push_str(&format!("{:>5}  {l}\n", i + 1));
        }
        if start > lines.len() {
            text.push_str("(past the end of the file)\n");
        }
        let mut notes = self.file_notes(&path);
        notes.retain(|n| !n.contains("compile_commands"));
        Ok(finish(text, notes))
    }

    fn search(&mut self, args: &Value, path: Option<PathBuf>, out: &Out) -> Result<String> {
        let pattern =
            args["pattern"].as_str().filter(|p| !p.is_empty()).context("give pattern: a regular expression")?;
        let limit = args["limit"].as_u64().map_or(50, |l| l.clamp(1, 500) as usize);
        let opts = Options {
            ignore_case: args["ignore_case"].as_bool().unwrap_or(false),
            fixed: args["fixed"].as_bool().unwrap_or(false),
            globs: args["glob"].as_str().map(|g| vec![g.to_string()]).unwrap_or_default(),
            types: Vec::new(),
        };
        let m = Matcher::new(pattern, &opts)?;
        let scope = path.map(|p| self.original(&p));
        let mut hits = Vec::new();
        let mut not_mirrored = 0;
        for at in locate::roots(&self.ws) {
            let source = at.root.source.to_string_lossy().into_owned();
            let prefix = match &scope {
                None if at.external => continue,
                None => String::new(),
                Some(s) => {
                    let s = s.to_string_lossy();
                    match paths::rest_under(&s, &source) {
                        Some(rest) => rest.replace('\\', "/"),
                        None if paths::is_under(&source, &s) => String::new(),
                        None => continue,
                    }
                }
            };
            let under = |rel: &str| prefix.is_empty() || rel == prefix || rel.starts_with(&format!("{prefix}/"));
            let index = crate::index::Index::load(&at.root.mirror, &at.root.source);
            not_mirrored += index.files.iter().filter(|(r, s)| s.mirror.is_none() && under(r)).count();
            let files: Vec<String> = search::mirrored(&at.root)
                .into_iter()
                .map(|(rel, _)| rel)
                .filter(|rel| under(rel) && m.wants(rel.strip_prefix(&prefix).unwrap_or(rel).trim_start_matches('/')))
                .collect();
            for hit in search::search(&at.root, &files, &m) {
                hits.push((paths::join_rel(&at.root.source, &hit.rel), hit.line, hit.text));
            }
        }
        let mut text = format!("{} matching lines", hits.len());
        if hits.len() > limit {
            text.push_str(&format!(" ({limit} shown)"));
        }
        text.push_str(" (searched in the mirrors, read right)\n");
        for (path, line, l) in hits.iter().take(limit) {
            text.push_str(&format!("{}:{line}: {}\n", out.path(path), clip(l.trim_end())));
        }
        let mut notes = Vec::new();
        if not_mirrored > 0 {
            notes.push(format!("{not_mirrored} files have no mirror and were not searched (mixenc doctor lists them)"));
        }
        Ok(finish(text, notes))
    }

    fn diagnose(&mut self, path: Option<PathBuf>, out: &Out) -> Result<String> {
        let report = doctor::run(&self.ws);
        let s = &report.summary;
        let mut text = format!(
            "workspace {}: {} broken, {} notices; {} files, {} mirrored\n",
            out.path(&self.ws.root),
            s.broken,
            s.notice,
            s.files,
            s.mirrored
        );
        for item in report.items.iter().filter(|i| i.severity != doctor::Severity::Ok) {
            let mark = if item.severity == doctor::Severity::Broken { "x" } else { "!" };
            let repo = item.repo.as_deref().map(|r| format!("{r}: ")).unwrap_or_default();
            text.push_str(&format!("{mark} {repo}{}\n", item.message));
        }
        if let Some(client) = self.client.as_ref().filter(|c| c.is_alive()) {
            text.push_str(&match client.index_state() {
                IndexState::Done => "clangd: running, index complete\n".to_string(),
                IndexState::Running(p, _) => format!("clangd: running, indexing {p}%\n"),
                IndexState::NotStarted => "clangd: running, index not started\n".to_string(),
            });
        } else {
            text.push_str("clangd: not started yet (the first question starts it)\n");
        }
        let mut notes = Vec::new();
        // A folder (or nothing) asks about the workspace only.
        if let Some(path) = path.filter(|p| p.is_file()) {
            let path = self.original(&path);
            let place = self.place(&path);
            let encoding = place.as_ref().and_then(|p| p.state()).map(|s| s.read_as.unwrap_or(s.judged).to_string());
            text.push_str(&format!(
                "\n{}: {}{}\n",
                out.path(&path),
                encoding.unwrap_or_else(|| "not in the mirror's index".into()),
                if is_source(&path) {
                    if self.in_db(&path) { ", in compile_commands.json" } else { ", not in compile_commands.json" }
                } else {
                    ""
                }
            ));
            notes.extend(self.file_notes(&path));
            self.open(&path)?;
            let client = self.client()?;
            let until = std::time::Instant::now() + Duration::from_secs(30);
            while client.diagnostics(&path).is_none() && std::time::Instant::now() < until && client.is_alive() {
                std::thread::sleep(Duration::from_millis(100));
            }
            match client.diagnostics(&path) {
                None => notes.push("clangd sent no diagnostics for it within 30 s".into()),
                Some(list) => {
                    let errors = list.iter().filter(|d| d["severity"].as_u64() == Some(1)).count();
                    text.push_str(&format!("clangd: {errors} errors, {} other diagnostics\n", list.len() - errors));
                    for d in list.iter().take(10) {
                        let sev = match d["severity"].as_u64() {
                            Some(1) => "error",
                            Some(2) => "warning",
                            _ => "note",
                        };
                        let line = d["range"]["start"]["line"].as_u64().unwrap_or(0) as usize;
                        text.push_str(&format!(
                            "  {sev} line {}: {}\n",
                            line + 1,
                            clip(d["message"].as_str().unwrap_or(""))
                        ));
                    }
                }
            }
        }
        Ok(finish(text, notes))
    }
}

/// A symbol from `workspace/symbol`.
struct Sym {
    name: String,
    container: String,
    kind: &'static str,
    path: PathBuf,
    line: usize,
}

impl Sym {
    fn from_lsp(v: &Value) -> Option<Sym> {
        let location = &v["location"];
        Some(Sym {
            name: v["name"].as_str()?.to_string(),
            container: v["containerName"].as_str().unwrap_or_default().trim_end_matches("::").to_string(),
            kind: kind_name(v["kind"].as_u64().unwrap_or(0)),
            path: uri::to_path(location["uri"].as_str()?)?,
            line: location["range"]["start"]["line"].as_u64()? as usize,
        })
    }

    fn qualified(&self) -> String {
        if self.container.is_empty() { self.name.clone() } else { format!("{}::{}", self.container, self.name) }
    }
}

/// Locations of an answer (`Location`, `LocationLink`, one or a list): (path, 0-based line).
fn locations(v: &Value) -> Vec<(PathBuf, usize)> {
    let list = match v {
        Value::Array(a) => a.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    };
    list.iter()
        .filter_map(|l| {
            let uri = l["uri"].as_str().or_else(|| l["targetUri"].as_str())?;
            let range = if l.get("targetSelectionRange").is_some() { &l["targetSelectionRange"] } else { &l["range"] };
            Some((uri::to_path(uri)?, range["start"]["line"].as_u64()? as usize))
        })
        .collect()
}

/// A hierarchy item's qualified name: clangd gives `Engine::Init`, or `Run` with the scope `Engine`
/// as its detail.
fn item_label(item: &Value) -> String {
    let name = item["name"].as_str().unwrap_or("?");
    match item["detail"].as_str().map(str::trim) {
        _ if name.contains("::") => name.to_string(),
        Some(d) if d.contains(name) && d.len() < 120 => d.to_string(),
        Some(d) if !d.is_empty() && d.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':') => {
            format!("{}::{name}", d.trim_end_matches(':'))
        }
        _ => name.to_string(),
    }
}

fn item_id(item: &Value) -> String {
    format!(
        "{}:{}:{}",
        item["uri"].as_str().unwrap_or_default().to_ascii_lowercase(),
        item["selectionRange"]["start"]["line"],
        item["name"]
    )
}

fn outline_lines(symbols: &[Value], level: usize, count: &mut usize, text: &mut String) {
    for s in symbols {
        if *count >= 300 {
            return;
        }
        *count += 1;
        // DocumentSymbol (range) or SymbolInformation (location.range).
        let range = if s.get("range").is_some() { &s["range"] } else { &s["location"]["range"] };
        let first = range["start"]["line"].as_u64().unwrap_or(0) + 1;
        let last = range["end"]["line"].as_u64().unwrap_or(0) + 1;
        let lines = if last > first { format!("{first}-{last}") } else { first.to_string() };
        let kind = kind_name(s["kind"].as_u64().unwrap_or(0));
        let detail = s["detail"]
            .as_str()
            .filter(|d| !d.is_empty() && *d != kind)
            .map(|d| format!("  {}", clip(d)))
            .unwrap_or_default();
        text.push_str(&format!(
            "{}{lines}  {kind} {}{detail}\n",
            "  ".repeat(level),
            s["name"].as_str().unwrap_or("?")
        ));
        if let Some(children) = s["children"].as_array() {
            outline_lines(children, level + 1, count, text);
        }
    }
}

fn kind_name(kind: u64) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum-member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type-parameter",
        _ => "symbol",
    }
}

/// Where `symbol`'s name is on `line` (1-based) or the lines near it: (0-based line, UTF-16 column).
fn find_name(lines: &[String], line: usize, symbol: &str) -> std::result::Result<(usize, usize), String> {
    let name = symbol.rsplit("::").next().unwrap_or(symbol).trim_end_matches("()").trim();
    if name.is_empty() {
        return Err(format!("{symbol:?} has no name to look for"));
    }
    if line == 0 || line > lines.len() {
        return Err(format!("line {line} is not in the file ({} lines)", lines.len()));
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let find = |text: &str| -> Option<usize> {
        text.match_indices(name).map(|(i, _)| i).find(|&i| {
            let before = text[..i].chars().next_back();
            let after = text[i + name.len()..].chars().next();
            !before.is_some_and(ident) && !after.is_some_and(ident) || !name.starts_with(ident)
        })
    };
    let line0 = line - 1;
    for distance in 0..=3usize {
        for l in [line0.checked_sub(distance), Some(line0 + distance)].into_iter().flatten() {
            if let Some(text) = lines.get(l)
                && let Some(i) = find(text)
            {
                return Ok((l, text[..i].encode_utf16().count()));
            }
        }
    }
    Err(format!("{name} is not on line {line} (it reads: {})", clip(&lines[line0])))
}

fn is_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("c" | "cc" | "cpp" | "cxx" | "c++")
    )
}

fn problem_text(problem: &crate::index::Problem) -> String {
    use crate::index::Problem;
    match problem {
        Problem::Undecided => "its encoding waits for a person's decision".into(),
        Problem::Skipped => "the settings file skips it".into(),
        Problem::Violation(d) => format!("its text broke an invariant: {d}"),
        Problem::Unreadable(d) => format!("unreadable: {d}"),
        Problem::WriteFailed(d) => format!("the mirror could not be written: {d}"),
    }
}

fn clip(s: &str) -> String {
    if s.chars().count() <= TEXT_LIMIT {
        return s.to_string();
    }
    let cut: String = s.chars().take(TEXT_LIMIT).collect();
    format!("{cut}…")
}

fn finish(mut text: String, notes: Vec<String>) -> String {
    let mut seen = HashSet::new();
    let notes: Vec<String> = notes.into_iter().filter(|n| seen.insert(n.clone())).collect();
    if !notes.is_empty() {
        text.push_str("\nnot vouched for:\n");
        for n in notes {
            text.push_str(&format!("- {n}\n"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_names_on_lines() {
        let lines: Vec<String> = [
            "int Base::Init() { return 0; }",
            "int Engine::Init() { const char* s = \"表示\"; return Helper(); }",
            "x",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(find_name(&lines, 1, "Base::Init"), Ok((0, 10)));
        // The column counts UTF-16 units: 表示 is two.
        assert_eq!(find_name(&lines, 2, "Helper"), Ok((1, 50)));
        // A name a line away is found there; a part of a longer name is not the name.
        assert_eq!(find_name(&lines, 3, "Engine::Init"), Ok((1, 12)));
        assert!(find_name(&lines, 3, "Hel").is_err());
        assert!(find_name(&lines, 9, "x").unwrap_err().contains("not in the file"));
    }

    #[test]
    fn reads_both_location_forms() {
        let v = json!([
            { "uri": "file:///C:/a.cpp", "range": { "start": { "line": 3, "character": 0 } } },
            { "targetUri": "file:///C:/b.h", "targetSelectionRange": { "start": { "line": 9, "character": 2 } }, "targetRange": { "start": { "line": 7 } } },
        ]);
        assert_eq!(locations(&v), vec![(PathBuf::from(r"C:\a.cpp"), 3), (PathBuf::from(r"C:\b.h"), 9)]);
        assert!(locations(&Value::Null).is_empty());
    }

    #[test]
    fn notes_come_last_once() {
        let text = finish("answer\n".into(), vec!["a".into(), "a".into(), "b".into()]);
        assert_eq!(text, "answer\n\nnot vouched for:\n- a\n- b\n");
    }
}
