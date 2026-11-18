use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;

use mixenc::config::{self, Workspace};
use mixenc::index::Problem;
use mixenc::mirror::Outcome;
use mixenc::{convert, copilot, doctor, edit, hooks, judge, mcp, paths, sync};

#[derive(Parser)]
#[command(
    name = "mixenc",
    version,
    about = "Lets clangd and AI agents read a C/C++ repository whose files mix CP932, UTF-8 and other encodings, through a UTF-8 mirror"
)]
struct Cli {
    /// Run as if started in this folder
    #[arg(short = 'C', global = true, value_name = "DIR")]
    dir: Option<PathBuf>,
    /// Print JSON (for editors and scripts)
    #[arg(long, global = true)]
    json: bool,
    /// The settings file (mixenc.json or <name>.mixenc.json) instead of the one found from the
    /// folder; `init` writes it; `workspace` takes several
    #[arg(long = "config", global = true, value_name = "FILE")]
    configs: Vec<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a settings file for a repository (or more): mixenc.json in the current folder, or the
    /// file --config names (such as <vault>/cloneA.mixenc.json; it may be kept in git, the
    /// mirrors then go outside it with --mirror). Registers it when a repository is outside its
    /// folder
    Init {
        /// A repository's folder (repeat for more; one per settings file is the usual)
        #[arg(long = "repo", required = true, value_name = "DIR")]
        repos: Vec<String>,
        /// Where the mirrors go (default: `mirror` beside the settings file; needed when that is in git)
        #[arg(long, value_name = "DIR")]
        mirror: Option<String>,
        /// The first repository's documents
        #[arg(long, value_name = "DIR")]
        wiki: Option<String>,
    },
    /// Register settings files (default: the one found from the folder), so that their
    /// repositories find them (Copilot's hooks, the commit gate, the analysis tools)
    Register { files: Vec<PathBuf> },
    /// Mirror the repositories and the include folders outside them, and write the mirror databases
    Sync {
        /// Only this repository (its name in mixenc.json)
        #[arg(long)]
        repo: Option<String>,
        /// Read every file again and write every mirror anew (not only the files that changed)
        #[arg(long)]
        rebuild: bool,
    },
    /// Check the mirrors, the judgements, the databases and the long paths (changes nothing)
    Doctor {
        /// Paths to show per finding
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// The deep checks: every translation unit through `clangd --check`, held against the
        /// baseline the first run takes (slow; progress on stderr)
        #[arg(long)]
        deep: bool,
        /// With --deep: clangd processes at once (default: half the processors)
        #[arg(long, requires = "deep")]
        jobs: Option<usize>,
        /// With --deep: take the baseline anew from this run
        #[arg(long, requires = "deep")]
        update_baseline: bool,
    },
    /// Add mixenc's items to a VS Code .code-workspace (made when it is not there): the
    /// repositories as folders, the settings files (--config, repeat for more), clangd through the
    /// entry, mixed and broken files read-only. Everything else in the file (other folders,
    /// settings, comments) stays. Adding the same again changes nothing.
    ///
    /// For an agent asked to set up a window: do not edit the .code-workspace or the settings files
    /// by hand. For each C/C++ repository, ask for (or agree on) the mirror folder (outside git)
    /// and where its settings file goes, then run
    ///   mixenc init --config <vault>/<repo>.mixenc.json --repo <repo> --mirror <folder>
    /// once per repository, then
    ///   mixenc workspace <the .code-workspace> --config <vault>/<repo>.mixenc.json ... --dry-run
    /// show the person the changes, and run it again without --dry-run.
    #[command(verbatim_doc_comment)]
    Workspace {
        /// The .code-workspace (default: <first repository>.code-workspace beside the settings file)
        file: Option<PathBuf>,
        /// Print the changes and write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the workspace that holds a folder or file (exit code 1 when none does)
    Where {
        /// The folder or file (default: the current folder)
        path: Option<PathBuf>,
    },
    /// List the files that are not plain UTF-8 or ASCII, with their judgement and state (--all: every file)
    Files {
        #[arg(long)]
        all: bool,
    },
    /// Decide how a file is read (the override table in mixenc.json): an encoding, or skip
    Override {
        file: PathBuf,
        /// cp932, utf8, euc-jp, utf16le, ... or skip
        #[arg(required_unless_present = "remove")]
        read_as: Option<String>,
        /// Take the file's entry out
        #[arg(long, conflicts_with = "read_as")]
        remove: bool,
    },
    /// Print a file's text read as its judgement, or as another encoding (to compare readings)
    Read {
        file: PathBuf,
        /// Read it as this encoding
        #[arg(long = "as", value_name = "ENCODING")]
        read_as: Option<String>,
        /// The first line (1-based)
        #[arg(long, default_value_t = 1)]
        start: usize,
        /// The last line
        #[arg(long)]
        end: Option<usize>,
    },
    /// Print the judgement of files
    Judge {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// The entry: start clangd on the mirror and stand between it and the client (set it as the
    /// editor's clangd); every argument goes on to clangd
    Clangd {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The front gate: replace text in a file of the workspace (or write a new one) in the file's
    /// own encoding, keeping every other line's bytes (what the MCP tools edit and create do)
    Edit {
        /// The file: the original, or its mirror
        path: PathBuf,
        /// The text to replace (it must be in the file once)
        #[arg(long, conflicts_with_all = ["old_file", "create"])]
        old: Option<String>,
        /// Read the text to replace from this UTF-8 file
        #[arg(long, value_name = "FILE", conflicts_with = "create")]
        old_file: Option<PathBuf>,
        /// The text to put in its place (or the new file's text)
        #[arg(long, conflicts_with = "new_file")]
        new: Option<String>,
        /// Read the new text from this UTF-8 file
        #[arg(long, value_name = "FILE")]
        new_file: Option<PathBuf>,
        /// Write a new file
        #[arg(long)]
        create: bool,
        /// For a file with no non-ASCII text so far: cp932, utf8-bom or utf8
        #[arg(long)]
        encoding: Option<String>,
    },
    /// Serve the analysis tools (clangd's answers, files read right) and the front gate's tools
    /// (edit, create) as an MCP server on stdin and stdout
    Mcp,
    /// The commit gate: a git pre-commit in each repository of the workspace
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    /// Copilot's gate (hooks), the analysis agent, and mixenc's MCP server and entry for Copilot
    Copilot {
        #[command(subcommand)]
        command: CopilotCommand,
    },
}

#[derive(Subcommand)]
enum CopilotCommand {
    /// Put the hooks, the analysis agent, the MCP server and the entry (as clangd) under
    /// ~/.copilot (COPILOT_HOME); settings of other tools there are kept
    Install {
        /// Copilot's folder (default: COPILOT_HOME, else ~/.copilot)
        #[arg(long, value_name = "DIR")]
        home: Option<PathBuf>,
        /// Leave Copilot CLI's clangd as it is
        #[arg(long)]
        no_lsp: bool,
    },
    /// Take out what install put in (clangd goes back to what it was)
    Uninstall {
        #[arg(long, value_name = "DIR")]
        home: Option<PathBuf>,
    },
    /// Show what is installed
    Status {
        #[arg(long, value_name = "DIR")]
        home: Option<PathBuf>,
    },
    /// Print the text for an orchestrator's instructions that introduces the analysis agent
    Intro {
        /// In Japanese
        #[arg(long)]
        ja: bool,
    },
    /// Answer one hook event (Copilot runs this; the event comes on stdin)
    Hook {
        /// sessionStart, preToolUse or postToolUse
        event: String,
    },
}

#[derive(Subcommand)]
enum HooksCommand {
    /// Install the pre-commit (a pre-commit already there is kept and runs first)
    Install {
        /// Only this repository (its name in mixenc.json)
        #[arg(long)]
        repo: Option<String>,
    },
    /// Remove the pre-commit and put back the one that was there before
    Uninstall {
        #[arg(long)]
        repo: Option<String>,
    },
    /// Show whether the pre-commit is installed
    Status {
        #[arg(long)]
        repo: Option<String>,
    },
    /// Check the staged files (what the pre-commit runs)
    PreCommit,
}

fn main() -> ExitCode {
    // Named clangd.exe (a copy put where a tool looks for clangd), mixenc is the entry.
    let named_clangd = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.eq_ignore_ascii_case("clangd")))
        .unwrap_or(false);
    if named_clangd {
        return match mixenc::entry::run(std::env::args().skip(1).collect()) {
            Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
            Err(err) => {
                eprintln!("mixenc: {err:#}");
                ExitCode::from(2)
            }
        };
    }
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            if cli.json {
                println!("{}", json!({ "error": format!("{err:#}") }));
            } else {
                eprintln!("error: {err:#}");
            }
            ExitCode::from(2)
        }
    }
}

/// The workspace of `--config`, else the one found from the folder.
fn workspace(cli: &Cli, dir: &std::path::Path) -> Result<Workspace> {
    match cli.configs.as_slice() {
        [] => Workspace::find(dir),
        [one] => Workspace::load_file(&dir.join(one)),
        _ => anyhow::bail!("this command takes one --config"),
    }
}

fn run(cli: &Cli) -> Result<u8> {
    let dir = match &cli.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    match &cli.command {
        Command::Init { repos, mirror, wiki } => {
            let file = match cli.configs.as_slice() {
                [] => None,
                [one] => Some(one.as_path()),
                _ => anyhow::bail!("init writes one settings file (one --config)"),
            };
            let init = config::Init { dir: &dir, file, repos, wiki: wiki.as_deref(), mirror: mirror.as_deref() };
            let file = config::init(&init)?;
            if cli.json {
                println!("{}", json!({ "config": paths::display(&file) }));
            } else {
                println!("wrote {}; next: mixenc sync --config {0}", paths::display(&file));
            }
            Ok(0)
        }
        Command::Register { files } => {
            let files: Vec<PathBuf> = if files.is_empty() {
                vec![workspace(cli, &dir)?.path]
            } else {
                files.iter().map(|f| dir.join(f)).collect()
            };
            let mut out = Vec::new();
            for f in &files {
                let ws = Workspace::load_file(f)?;
                let added = config::register(&ws.path)?;
                out.push(json!({ "config": paths::display(&ws.path), "added": added }));
                if !cli.json {
                    println!(
                        "{} {}",
                        if added { "registered" } else { "already registered:" },
                        paths::display(&ws.path)
                    );
                }
            }
            if cli.json {
                println!("{}", json!({ "registered": out }));
            }
            Ok(0)
        }
        Command::Sync { repo, rebuild } => {
            let ws = workspace(cli, &dir)?;
            if *rebuild {
                // Without its index a mirror is read and written anew, file by file (only a mirror
                // of this original: another one's is not taken).
                for r in ws.repos() {
                    mixenc::mirror::check_owner(&mixenc::mirror::Root { source: r.source, mirror: r.mirror })?;
                }
                for at in mixenc::locate::roots(&ws) {
                    mixenc::mirror::check_owner(&at.root)?;
                    if repo.as_deref().is_none_or(|n| at.repo.name.eq_ignore_ascii_case(n)) {
                        let index = mixenc::index::Index::path(&at.root.mirror);
                        if index.is_file() {
                            std::fs::remove_file(&index)?;
                        }
                    }
                }
            }
            let results = sync::sync(&ws, repo.as_deref())?;
            print_sync(&ws, &results, cli.json);
            let not_mirrored = results.iter().any(|r| {
                std::iter::once(&r.repo)
                    .chain(r.external.iter().map(|(_, o)| o))
                    .any(|o| o.not_mirrored.iter().any(|(_, p)| !matches!(p, Problem::Skipped | Problem::Undecided)))
            });
            Ok(u8::from(not_mirrored || results.iter().any(|r| !r.state.db_problems.is_empty())))
        }
        Command::Doctor { limit, deep, jobs, update_baseline } => {
            let ws = workspace(cli, &dir)?;
            let report = if *deep {
                let opts = mixenc::deep::Options { jobs: *jobs, update_baseline: *update_baseline };
                let json = cli.json;
                mixenc::deep::run(&ws, &opts, &move |done, total, file| {
                    if json {
                        eprintln!("progress {done}/{total} {file}");
                    } else {
                        eprintln!("[{done}/{total}] {file}");
                    }
                })?
            } else {
                doctor::run(&ws)
            };
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report.to_text(*limit));
            }
            Ok(u8::from(report.summary.broken > 0))
        }
        Command::Clangd { args } => {
            let code = mixenc::entry::run(args.clone())?;
            Ok(u8::try_from(code).unwrap_or(1))
        }
        Command::Edit { path, old, old_file, new, new_file, create, encoding } => {
            let path = if path.is_absolute() { path.clone() } else { dir.join(path) };
            let encoding =
                encoding.as_deref().map(str::parse::<judge::Encoding>).transpose().map_err(anyhow::Error::msg)?;
            let new = text_arg(new, new_file)?.ok_or_else(|| anyhow::anyhow!("--new or --new-file is needed"))?;
            let outcome = if *create {
                edit::create(&path, &new, encoding)?
            } else {
                let old = text_arg(old, old_file)?
                    .ok_or_else(|| anyhow::anyhow!("--old or --old-file is needed (or --create)"))?;
                edit::replace(&path, &old, &new, encoding)?
            };
            if cli.json {
                let (first, last) = outcome.lines.unzip();
                println!(
                    "{}",
                    json!({
                        "path": paths::display(&outcome.path),
                        "encoding": outcome.encoding.name(),
                        "first_line": first,
                        "last_line": last,
                        "removed": outcome.removed,
                        "notes": outcome.notes,
                    })
                );
            } else {
                println!("{}", outcome.summary());
            }
            Ok(0)
        }
        Command::Mcp => {
            let workspaces = cli.configs.iter().map(|c| Workspace::load_file(&dir.join(c))).collect::<Result<_>>()?;
            mcp::serve(workspaces)?;
            Ok(0)
        }
        Command::Hooks { command } => run_hooks(cli, &dir, command),
        Command::Workspace { file, dry_run } => {
            let workspaces: Vec<Workspace> = if cli.configs.is_empty() {
                vec![Workspace::find(&dir)?]
            } else {
                cli.configs.iter().map(|c| Workspace::load_file(&dir.join(c))).collect::<Result<_>>()?
            };
            let file = match file {
                Some(f) => dir.join(f),
                None => {
                    let first = &workspaces[0];
                    let name = first.repos().first().map(|r| r.name.clone()).unwrap_or_else(|| "mixenc".into());
                    first.root.join(format!("{name}.code-workspace"))
                }
            };
            let added = mixenc::codews::add(&file, &workspaces, &std::env::current_exe()?, *dry_run)?;
            // The repositories outside their settings file's folder find it through the register.
            if !*dry_run {
                for ws in &workspaces {
                    if ws.needs_register() {
                        config::register(&ws.path)?;
                    }
                }
            }
            if cli.json {
                println!(
                    "{}",
                    json!({
                        "path": paths::display(&added.path),
                        "changes": added.changes,
                        "written": added.written,
                        "copilot": added.copilot,
                    })
                );
            } else {
                let what = if *dry_run { "would change" } else { "changed" };
                if added.changes.is_empty() {
                    println!("{} has everything already", paths::display(&added.path));
                } else {
                    println!("{what} {}:", paths::display(&added.path));
                    for c in &added.changes {
                        println!("  {c}");
                    }
                }
                if let Some(c) = &added.copilot {
                    println!("Copilot CLI with the window's other folders at hand (in a repository): {c}");
                }
            }
            Ok(0)
        }
        Command::Where { path } => {
            let path =
                path.as_ref().map_or_else(|| dir.clone(), |p| if p.is_absolute() { p.clone() } else { dir.join(p) });
            let Ok(ws) = (if cli.configs.is_empty() { Workspace::find(&path) } else { workspace(cli, &dir) }) else {
                if cli.json {
                    println!("{}", json!({ "workspace": null }));
                } else {
                    println!("no mixenc workspace holds {}", paths::display(&path));
                }
                return Ok(1);
            };
            let key = paths::canonical(&std::path::absolute(&path)?).to_string_lossy().into_owned();
            let repo = ws.repos().into_iter().find(|r| paths::is_under(&key, &r.source.to_string_lossy()));
            if cli.json {
                println!(
                    "{}",
                    json!({
                        "workspace": paths::display(&ws.root),
                        "config": paths::display(&ws.path),
                        "repo": repo.as_ref().map(|r| r.name.clone()),
                        "repo_path": repo.as_ref().map(|r| paths::display(&r.source)),
                        "mirror": repo.as_ref().map(|r| paths::display(&r.mirror)),
                    })
                );
            } else {
                println!("{}", paths::display(&ws.path));
            }
            Ok(0)
        }
        Command::Files { all } => {
            let ws = workspace(cli, &dir)?;
            let mut files = Vec::new();
            let mut roots = Vec::new();
            for at in mixenc::locate::roots(&ws) {
                roots.push(paths::display(&at.root.source));
                let index = mixenc::index::Index::load(&at.root.mirror, &at.root.source);
                for (rel, s) in index.files {
                    let read = s.read_as.unwrap_or(s.judged);
                    if !*all && matches!(read, judge::Encoding::Ascii | judge::Encoding::Utf8) && s.problem.is_none() {
                        continue;
                    }
                    let problem = s.problem.as_ref().map(|p| serde_json::to_value(p).unwrap_or_default());
                    // A mixed or broken file opens in the encoding most of its lines are in.
                    let major = matches!(read, judge::Encoding::Mixed | judge::Encoding::Broken)
                        .then(|| std::fs::read(paths::join_rel(&at.root.source, &rel)).ok())
                        .flatten()
                        .and_then(|b| mixenc::doc::parse(&b, read))
                        .map(|d| if d.major() == mixenc::doc::LineEnc::Cp932 { "cp932" } else { "utf8" });
                    files.push(json!({
                        "path": paths::display(&paths::join_rel(&at.root.source, &rel)),
                        "repo": at.repo.name,
                        "external": at.external,
                        "judged": s.judged.name(),
                        "read_as": s.read_as.map(|e| e.name()),
                        "mirrored": s.mirror.is_some(),
                        "problem": problem,
                        "readonly": matches!(s.judged, judge::Encoding::Mixed | judge::Encoding::Broken) && s.mirror.is_some(),
                        "major": major,
                    }));
                }
            }
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&json!({ "files": files, "roots": roots }))?);
            } else {
                for f in &files {
                    let read_as = f["read_as"]
                        .as_str()
                        .filter(|r| *r != f["judged"])
                        .map(|r| format!(" (read as {r})"))
                        .unwrap_or_default();
                    let state = if f["mirrored"] == true { "" } else { "  not mirrored" };
                    println!(
                        "{}{read_as}\t{}{state}",
                        f["judged"].as_str().unwrap_or(""),
                        f["path"].as_str().unwrap_or("")
                    );
                }
            }
            Ok(0)
        }
        Command::Override { file, read_as, remove } => {
            let path = if file.is_absolute() { file.clone() } else { dir.join(file) };
            let place = mixenc::locate::find(&path, None)
                .ok_or_else(|| anyhow::anyhow!("{} is not in a folder a workspace mirrors", paths::display(&path)))?;
            let original = place.original();
            let read_as = match (remove, read_as.as_deref()) {
                (true, _) => None,
                (false, Some("skip")) => Some(config::ReadAs::Skip),
                (false, Some(name)) => {
                    let e: judge::Encoding = name.parse().map_err(anyhow::Error::msg)?;
                    if !e.is_decided() || e == judge::Encoding::Binary {
                        anyhow::bail!("{name} is a judgement, not an encoding to read a file as");
                    }
                    Some(config::ReadAs::Encoding(e))
                }
                (false, None) => anyhow::bail!("give the encoding (or skip), or --remove"),
            };
            let judged = std::fs::read(&original).ok().map(|b| judge::judge(&b));
            let changed = config::set_override(&place.ws, &original, read_as, judged)?;
            // Mirror the file the new way at once.
            let ws = Workspace::load_file(&place.ws.path)?;
            if let Some(place) = mixenc::locate::find(&original, Some(&ws)) {
                place.sync()?;
            }
            println!("{} {}", if changed { "updated" } else { "no entry for" }, ws.display_path(&original));
            Ok(0)
        }
        Command::Read { file, read_as, start, end } => {
            let path = if file.is_absolute() { file.clone() } else { dir.join(file) };
            let bytes = std::fs::read(&path).map_err(|e| anyhow::anyhow!("{}: {e}", paths::display(&path)))?;
            let judged = judge::judge(&bytes);
            let wanted = match read_as {
                Some(name) => name.parse::<judge::Encoding>().map_err(anyhow::Error::msg)?,
                None => mixenc::locate::find(&path, None)
                    .and_then(|p| p.read_as())
                    .and_then(|r| match r {
                        config::ReadAs::Encoding(e) => Some(e),
                        config::ReadAs::Skip => None,
                    })
                    .unwrap_or(judged),
            };
            let wanted = match wanted {
                judge::Encoding::AmbiguousCp932EucJp => judge::Encoding::Cp932,
                e => e,
            };
            let doc = mixenc::doc::parse(&bytes, wanted)
                .ok_or_else(|| anyhow::anyhow!("{} cannot be read as {wanted}", paths::display(&path)))?;
            let last = end.unwrap_or(doc.lines.len()).min(doc.lines.len());
            let lines: Vec<&str> =
                doc.lines.iter().take(last).skip(start.saturating_sub(1)).map(|l| l.text.as_str()).collect();
            if cli.json {
                let unread = doc.lines.iter().filter(|l| !l.ok).count();
                println!(
                    "{}",
                    json!({ "path": paths::display(&path), "judged": judged.name(), "read_as": wanted.name(), "start": start, "lines": lines, "unreadable_lines": unread })
                );
            } else {
                for (i, l) in lines.iter().enumerate() {
                    println!("{:>5}  {l}", start + i);
                }
            }
            Ok(0)
        }
        Command::Copilot { command } => run_copilot(cli, command),
        Command::Judge { files } => {
            let mut out = Vec::new();
            for file in files {
                let path = if file.is_absolute() { file.clone() } else { dir.join(file) };
                let bytes = std::fs::read(&path).map_err(|e| anyhow::anyhow!("{}: {e}", paths::display(&path)))?;
                let judged = judge::judge(&bytes);
                out.push((path, judged, convert::traps(&bytes, judged)));
            }
            if cli.json {
                let files: Vec<_> = out
                    .iter()
                    .map(|(p, j, t)| json!({ "path": paths::display(p), "judged": j.name(), "traps": t }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&files)?);
            } else {
                for (path, judged, traps) in &out {
                    let traps = if *traps > 0 {
                        format!(" ({traps} lines a UTF-8 reader would splice)")
                    } else {
                        String::new()
                    };
                    println!("{judged}\t{}{traps}", paths::display(path));
                }
            }
            Ok(0)
        }
    }
}

/// A text given on the command line, or read from a UTF-8 file.
fn text_arg(text: &Option<String>, file: &Option<PathBuf>) -> Result<Option<String>> {
    match (text, file) {
        (Some(text), _) => Ok(Some(text.clone())),
        (None, Some(file)) => {
            let bytes = std::fs::read(file).map_err(|e| anyhow::anyhow!("{}: {e}", paths::display(file)))?;
            let bytes = bytes.strip_prefix(mixenc::text::UTF8_BOM).unwrap_or(&bytes).to_vec();
            String::from_utf8(bytes).map(Some).map_err(|_| anyhow::anyhow!("{} is not UTF-8", paths::display(file)))
        }
        (None, None) => Ok(None),
    }
}

fn run_copilot(cli: &Cli, command: &CopilotCommand) -> Result<u8> {
    let home_of = |h: &Option<PathBuf>| h.clone().unwrap_or_else(copilot::home);
    let done = match command {
        CopilotCommand::Hook { event } => {
            // Whatever happens, the hook exits 0: a failing preToolUse hook denies every tool.
            let mut input = Vec::new();
            let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut input);
            let answer = std::panic::catch_unwind(|| copilot::hook(event, &input)).ok().flatten();
            if let Some(answer) = answer {
                println!("{answer}");
            }
            return Ok(0);
        }
        CopilotCommand::Intro { ja } => {
            print!("{}", if *ja { copilot::INTRO_JA } else { copilot::INTRO });
            return Ok(0);
        }
        CopilotCommand::Status { home } => {
            let pieces = copilot::status(&home_of(home));
            if cli.json {
                let list: Vec<_> = pieces
                    .iter()
                    .map(|p| json!({ "name": p.name, "path": paths::display(&p.path), "ok": p.ok, "status": p.text }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&json!({ "pieces": list }))?);
            } else {
                for p in &pieces {
                    println!("{}: {} ({})", p.name, p.text, paths::display(&p.path));
                }
            }
            return Ok(u8::from(!pieces.iter().all(|p| p.ok)));
        }
        CopilotCommand::Install { home, no_lsp } => {
            copilot::install(&home_of(home), &std::env::current_exe()?, !no_lsp)?
        }
        CopilotCommand::Uninstall { home } => copilot::uninstall(&home_of(home))?,
    };
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&json!({ "done": done }))?);
    } else {
        for line in &done {
            println!("{line}");
        }
        if done.is_empty() {
            println!("nothing to do");
        }
        if matches!(command, CopilotCommand::Install { .. }) {
            println!(
                "Copilot reads these when it starts. VS Code's Copilot reads the hooks and the agent too; add the MCP server there with:\n  code --add-mcp \"{{\\\"name\\\":\\\"mixenc\\\",\\\"command\\\":\\\"{}\\\",\\\"args\\\":[\\\"mcp\\\"]}}\"",
                paths::forward(&std::env::current_exe()?.to_string_lossy())
            );
        }
    }
    Ok(0)
}

fn run_hooks(cli: &Cli, dir: &std::path::Path, command: &HooksCommand) -> Result<u8> {
    let only = match command {
        HooksCommand::PreCommit => {
            return match hooks::check_staged(dir) {
                Ok(stopped) if stopped.is_empty() => Ok(0),
                Ok(stopped) => {
                    eprint!("{}", hooks::report(&stopped));
                    Ok(1)
                }
                Err(err) => {
                    eprintln!("mixenc pre-commit: the commit gate could not check this commit: {err:#}");
                    eprintln!("(git commit --no-verify skips the gate)");
                    Ok(2)
                }
            };
        }
        HooksCommand::Install { repo } | HooksCommand::Uninstall { repo } | HooksCommand::Status { repo } => repo,
    };
    let ws = workspace(cli, dir)?;
    let repos: Vec<_> =
        ws.repos().into_iter().filter(|r| only.as_deref().is_none_or(|n| r.name.eq_ignore_ascii_case(n))).collect();
    if repos.is_empty() {
        anyhow::bail!("no repository {} in {}", only.as_deref().unwrap_or(""), paths::display(&ws.file()));
    }
    let exe = std::env::current_exe()?;
    let mut code = 0;
    let mut out = Vec::new();
    for repo in &repos {
        let status = match command {
            HooksCommand::Install { .. } => hooks::install(&repo.source, &exe)?,
            HooksCommand::Uninstall { .. } => hooks::uninstall(&repo.source)?,
            _ => hooks::status(&repo.source)?,
        };
        let text = match &status {
            hooks::Status::Installed { chained: true } => {
                "installed (the pre-commit that was there before runs first)".to_string()
            }
            hooks::Status::Installed { chained: false } => "installed".to_string(),
            hooks::Status::Missing { other: true } => "not installed (another pre-commit is there)".to_string(),
            hooks::Status::Missing { other: false } => "not installed".to_string(),
            hooks::Status::HooksPath(p) => {
                code = 1;
                format!("not installed: core.hooksPath is set ({p}), so the hooks are managed there")
            }
        };
        out.push((repo.name.clone(), matches!(status, hooks::Status::Installed { .. }), text));
    }
    if cli.json {
        let repos: Vec<_> = out
            .iter()
            .map(|(name, installed, text)| json!({ "name": name, "installed": installed, "status": text }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&json!({ "repos": repos }))?);
    } else {
        for (name, _, text) in &out {
            println!("{name}: {text}");
        }
    }
    Ok(code)
}

fn print_sync(ws: &Workspace, results: &[sync::RepoSync], json: bool) {
    if json {
        let repos: Vec<_> = results
            .iter()
            .map(|r| {
                let mut roots = vec![(r.state.source.clone(), &r.repo)];
                roots.extend(r.external.iter().map(|(root, o)| (paths::display(&root.source), o)));
                let roots: Vec<_> = roots
                    .into_iter()
                    .map(|(source, o)| {
                        json!({
                            "source": source,
                            "files": o.files,
                            "written": o.written,
                            "removed": o.removed,
                            "repaired": o.repaired,
                            "not_mirrored": o.not_mirrored.iter().map(|(rel, p)| json!({ "path": rel, "problem": p })).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                json!({
                    "name": r.name,
                    "mirror": r.state.mirror,
                    "roots": roots,
                    "path_mappings": r.state.path_mappings,
                    "db": r.state.db,
                    "db_problems": r.state.db_problems,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json!({ "repos": repos })).unwrap_or_default());
        return;
    }
    for r in results {
        let line = |what: &str, o: &Outcome| {
            let repaired = if o.repaired > 0 { format!(", {} repaired", o.repaired) } else { String::new() };
            let not_mirrored = if o.not_mirrored.is_empty() {
                String::new()
            } else {
                format!(", {} not mirrored", o.not_mirrored.len())
            };
            println!(
                "{}{what}: {} files, {} written, {} removed{repaired}{not_mirrored}",
                r.name, o.files, o.written, o.removed
            );
        };
        line("", &r.repo);
        for (root, o) in &r.external {
            line(&format!(" (outside: {})", ws.display_path(&root.source)), o);
        }
        match &r.state.db {
            Some(db) => match &db.mirror {
                Some(m) => println!("{}: mirror database {m} ({} entries)", r.name, db.entries),
                None => println!("{}: mirror database not written", r.name),
            },
            None => println!("{}: no compile_commands.json found", r.name),
        }
        for p in &r.state.db_problems {
            println!("  {p}");
        }
    }
    println!("mixenc doctor shows what needs doing");
}
