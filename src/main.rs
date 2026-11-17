use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;

use mixenc::config::{self, Workspace};
use mixenc::index::Problem;
use mixenc::mirror::Outcome;
use mixenc::{convert, doctor, judge, paths, sync};

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
}

fn main() -> ExitCode {
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
