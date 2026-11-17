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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write mixenc.json in the current folder (the workspace), for one or more repositories
    Init {
        /// A repository's folder (repeat for more)
        #[arg(long = "repo", required = true, value_name = "DIR")]
        repos: Vec<String>,
        /// The first repository's documents
        #[arg(long, value_name = "DIR")]
        wiki: Option<String>,
    },
    /// Mirror the repositories and the include folders outside them, and write the mirror databases
    Sync {
        /// Only this repository (its name in mixenc.json)
        #[arg(long)]
        repo: Option<String>,
    },
    /// Check the mirrors, the judgements, the databases and the long paths (changes nothing)
    Doctor {
        /// Paths to show per finding
        #[arg(long, default_value_t = 10)]
        limit: usize,
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

fn run(cli: &Cli) -> Result<u8> {
    let dir = match &cli.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    match &cli.command {
        Command::Init { repos, wiki } => {
            let file = config::init(&dir, repos, wiki.as_deref())?;
            if cli.json {
                println!("{}", json!({ "config": paths::display(&file) }));
            } else {
                println!("wrote {}; next: mixenc sync", paths::display(&file));
            }
            Ok(0)
        }
        Command::Sync { repo } => {
            let ws = Workspace::find(&dir)?;
            let results = sync::sync(&ws, repo.as_deref())?;
            print_sync(&ws, &results, cli.json);
            let not_mirrored = results.iter().any(|r| {
                std::iter::once(&r.repo)
                    .chain(r.external.iter().map(|(_, o)| o))
                    .any(|o| o.not_mirrored.iter().any(|(_, p)| !matches!(p, Problem::Skipped | Problem::Undecided)))
            });
            Ok(u8::from(not_mirrored || results.iter().any(|r| !r.state.db_problems.is_empty())))
        }
        Command::Doctor { limit } => {
            let ws = Workspace::find(&dir)?;
            let report = doctor::run(&ws);
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
            mcp::serve()?;
            Ok(0)
        }
        Command::Hooks { command } => run_hooks(cli, &dir, command),
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
    let ws = Workspace::find(dir)?;
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
