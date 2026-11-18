# mixenc

English | [日本語](README.ja.md)

Lets clangd and AI agents read a C/C++ repository whose files mix Shift_JIS (CP932), UTF-8 and
other encodings, without misreading a line and without losing a file.

clangd reads every source as UTF-8. In CP932, characters such as 表, 能 and ソ end in the byte
`0x5C`, which is `\` in ASCII, so a comment that ends in one of them becomes a line splice to a
UTF-8 reader, and the next line (often a declaration) disappears:

```cpp
// 表示する設定の表        <- ends in 95 5C: "\" to a UTF-8 reader
int hidden(int);          <- swallowed into the comment; clangd: "use of undeclared identifier"
```

mixenc keeps a **mirror** of the repository in UTF-8, outside it, with the same lines and line
breaks, and a **mirror database** (`compile_commands.json` pointing into the mirror). clangd reads
the mirror (`--compile-commands-dir`), and `--path-mappings` turns every path back, so editors
and agents keep working on the originals, in their own encodings.

- **One judgement per file.** BOMs, UTF-16 with or without a BOM, ISO-2022-JP, CP932 read two
  bytes at a time (as MSVC does), UTF-8, EUC-JP, UTF-8 and CP932 lines mixed in one file, broken
  bytes. A file two encodings both read is listed, not guessed at.
- **Checked before it is written.** A mirror file must be strict UTF-8 and have the original's
  lines one to one, ending in `\` exactly where the original really does. A file that fails gets
  no mirror and is listed, so clangd never reads a wrong one.
- **Machines check, not attention.** `mixenc doctor` checks the mirror against its index (nothing
  changed by hand, nothing behind the originals), the judgements, the mirror database (no path
  leads to an original, no charset option left), the toolchain's headers and long paths.
- **Long paths.** Paths of about 950 characters work; the doctor lists file names clangd cannot
  index persistently (235 characters and more).
- **Nothing leaves your machine.** No telemetry, no network access. The doctor's report has paths
  in it; it stays where it is made.

**Status: all six stages built.** mixenc judges, mirrors, writes the mirror database, checks
(the short doctor and the deep one), has the clangd entry (`mixenc clangd`), the analysis tools
(clangd's answers through MCP) and an analysis agent, the front gate (`mixenc edit` and the MCP
tools that write originals in their own encoding), the commit gate (a git pre-commit), Copilot's
gate (hooks for VS Code's Copilot and Copilot CLI), and a VS Code extension. Its settings files can
be kept in git with the mirrors outside it, and it adds its items to your `.code-workspace`.

## Requirements

- Windows 10 or 11 (mixenc reads CP932 with Windows' own table)
- git
- A `compile_commands.json` for the repository; [lazymsbuild](https://github.com/6r1ntry52o/lazymsbuild)
  keeps one for a Visual Studio solution, and mixenc finds it through `lazymsbuild path`
- clangd, to use the result

## Install

- **mixenc**: from [Releases](https://github.com/6r1ntry52o/mixenc/releases), the zip for your PC
  (`x86_64` or `aarch64`), checked with `SHA256SUMS`; put `mixenc.exe` on `PATH`. Or from source:
  `cargo install --git https://github.com/6r1ntry52o/mixenc` (Rust 1.89 or later).
- **The VS Code extension**: `mixenc-vscode-<version>.vsix` from the same release,
  `code --install-extension mixenc-vscode-<version>.vsix`.

## Getting started

A **settings file** (`mixenc.json`, or `<name>.mixenc.json`) says where a repository is and where its
mirror goes. One per repository is the usual. Keep the settings files with your notes (a vault in
git is fine) and the mirrors outside git; no folder layout is required:

```text
C:/src/cloneA/                 the repository (originals; people and agents edit these)
D:/mirror/cloneA/              its mirror, the mirror database, clangd's index
D:/mirror/_ext/<name>/         mirrors of include folders outside the repository
D:/mirror/.mixenc/             mixenc's own state and logs
<vault>/                       in git: only the settings and the window
├─ cloneA.mixenc.json          { "mirror": "D:/mirror", "repos": [{ "path": "C:/src/cloneA" }] }
├─ cloneB.mixenc.json
└─ dev.code-workspace          your window; mixenc adds its items (mixenc.configs, clangd, ...)
```

1. `mixenc init --config <vault>/cloneA.mixenc.json --repo C:/src/cloneA --mirror D:/mirror` writes
   the settings file and registers it (so that the repository finds it). Without `--config`,
   `mixenc.json` goes to the current folder; without `--mirror`, the mirrors go to `mirror` beside it,
   which is refused when that is in a git work tree.
2. `mixenc sync` (in the repository, or with `--config`) mirrors it and writes the mirror database.
3. `mixenc doctor` lists what needs doing (exit code 1 when something is broken).
4. Point your editor's or agent's clangd at the entry, `mixenc clangd` (below). It syncs at every
   start and while it runs, so `mixenc sync` by hand is only needed without it.
5. Install the commit gate: `mixenc hooks install`.
6. For Copilot (VS Code and Copilot CLI): `mixenc copilot install` puts the hooks, the analysis agent,
   the MCP server and the entry under `~/.copilot` (below).
7. For VS Code: `mixenc workspace <vault>/dev.code-workspace --config <vault>/cloneA.mixenc.json
   --config <vault>/cloneB.mixenc.json` adds mixenc's items to your `.code-workspace` (made when it
   is not there; `--dry-run` shows the changes first). The extension's "mixenc: Add a repository to
   this window" does steps 1, 2 and 7 from dialogs.

Every command finds the settings file from the folder it runs in (a repository, a mirror, or the
settings file's folder), or takes `--config <file>`.

An agent asked to set up a window does the same with these commands, and never edits the
`.code-workspace` or a settings file by hand (`mixenc workspace --help` says how).

## The clangd entry

`mixenc clangd [clangd's arguments]` starts clangd on the mirror and stands between it and the
client:

- It serves the settings files given with `--mixenc-config <file>` (repeat it: an editor starts
  one clangd for all the folders of a window), else the one of each folder the client opens (a
  settings file there or above, or a registered one), syncs, and starts clangd with
  `--compile-commands-dir` (one repository) and `--path-mappings`. Settings files whose mirrors
  overlap are left out, and it says so. Without a settings file it passes everything through to
  clangd.
- Before a file is opened or saved, it mirrors that file. It watches the repositories and the
  database, and syncs after a change has settled (a checkout, another tool).
- A client that reads CP932 files as UTF-8 (agents' LSP clients do) sends the 0x5C trap along:
  such a text is replaced with the mirror's, and from then on that client's columns are moved
  between its reading and the mirror's, both ways (the lines are the same).
- Files that are not mirrored are reported with `window/showMessage`; analysis goes on. If the
  entry itself fails on a message, it passes that message through unchanged and says so.

Which clangd it starts: `MIXENC_CLANGD`, else `"clangd"` in the settings file, else `clangd.exe` on
`PATH`, else the one vscode-clangd installed, else the one Neovim's Mason installed.

It keeps a log in `<mirror>\.mixenc\entry.log` (on this PC only): how each document came (read right,
misread and replaced), the diagnostics counts, and where definitions and references lead (a
mirror path there would be a bug). Both VS Code without a `files.encoding` and Copilot CLI send CP932
files misread as UTF-8 (measured); the entry replaces them.

- **VS Code** (vscode-clangd): `"clangd.path": "C:\\Tools\\mixenc.exe"` and
  `"clangd.arguments": ["clangd", "--background-index", "--mixenc-config", "<settings file>", ...]`
  (`mixenc workspace` writes both).
- **Copilot CLI**: in `~/.copilot/lsp-config.json`, `"command": "mixenc"`,
  `"args": ["clangd", "--background-index"]`.
- **Anything that runs `clangd`**: a copy of `mixenc.exe` named `clangd.exe`, earlier on `PATH` than
  the real one, is the entry (it skips itself when it looks for clangd).

## The analysis tools (`mixenc mcp`)

`mixenc mcp` is an MCP server. Its analysis tools ask clangd (through the entry, which it starts)
and answer short, with the originals' paths and every file read right:

| Tool | Answers |
|---|---|
| `find_symbol` | kind, qualified name and path:line of the symbols a name (or part of one) finds in clangd's index |
| `definition` | where a symbol is defined and declared, with the lines around |
| `references` | how many uses, and their lines per file (no text) |
| `call_hierarchy` | who calls a function (incoming) or what it calls (outgoing), as a tree, levels deep, with the lines of the calls |
| `type_hierarchy` | base and derived classes, as trees |
| `outline` | a file's declarations and their lines, nested |
| `read_lines` | lines of a file, decoded right (CP932, mixed, UTF-16) |
| `search` | a regular expression over the mirrors, answered with the originals' path:line |
| `diagnose` | the doctor's short report, clangd's index, and for a file: its encoding, whether it is in the database, clangd's errors |

A question about a place takes the symbol's name, and the file and line where it is written when the
name stands for several symbols; nobody counts columns. A tool that cannot vouch for its answer (the
file is not in `compile_commands.json`, clangd is still indexing, the file is not mirrored) says so
under "not vouched for".

**The analysis agent** (`mixenc-analyst`, installed by `mixenc copilot install`) is a Copilot custom
agent that has only these tools. It works in order: find the symbol, then definitions, references
and call and type hierarchies, then read only the lines it needs; it searches text only for strings,
comments and files clangd does not compile. It answers with a conclusion, the evidence as path:line,
and what it did not check. `mixenc copilot intro` (`--ja` in Japanese) prints a short text to put in
an orchestrator's instructions so that it hands such questions to the agent.

## The front gate (`mixenc edit`, `mixenc mcp`)

An editor or an agent that reads a CP932 file through Unicode and writes it back changes bytes it
was never asked to: `Ⅰ`, `№` and `㈱` have an NEC and an IBM form, and a file read in the wrong
encoding comes back broken. The front gate is how an agent writes an original instead: it replaces
text in a file and writes the file back in its own encoding.

- Only the lines whose text changed are encoded (as Visual Studio writes CP932); every other line
  keeps its bytes and its line break. A mixed file's changed lines keep their line's encoding, and
  its new lines get the encoding most of its lines are in.
- A file with no non-ASCII text so far gets the encoding the files around it use: those of the same
  project in the database, else the same folder, else the repository; the most common of `cp932`,
  `utf8-bom` and `utf8` (a tie: `cp932`). UTF-8 without a BOM only where the project compiles with
  `/utf-8`; where it is the most common without that, the tool asks for a person's decision.
- A character the encoding has no form for (`😀` in CP932) stops the edit and is named with its
  line; for the characters JIS and CP932 map apart (`〜`), the CP932 one is named too.
- A path in the mirror stands for its original. The text to replace must be in the file once; line
  breaks may be given as `\n`.
- Before anything is written, the result must read back as the text asked for and pass the commit
  gate's check, and the original must be as it was when it was read. The mirror follows.

`mixenc mcp` serves this as two more MCP tools, `edit` (path, old_string, new_string) and `create`
(path, content), for VS Code's Copilot and Copilot CLI:

- **VS Code**: in `mcp.json` (MCP: Open User Configuration),
  `"servers": { "mixenc": { "type": "stdio", "command": "C:\\Tools\\mixenc.exe", "args": ["mcp"] } }`.
- **Copilot CLI**: `mixenc copilot install` registers it (or `copilot mcp add mixenc -- mixenc mcp`).

`mixenc edit <file> --old <text> --new <text>` (or `--old-file`/`--new-file`, `--create`) does the
same from the command line.

## The commit gate (`mixenc hooks`)

`mixenc hooks install` puts a git pre-commit into each repository of the settings file. For every
staged file it checks, against HEAD:

- the encoding stays (a `cp932` file does not become `utf8`); a file with no non-ASCII text so far
  may get only a form MSVC reads right where it is (as above);
- a line whose text did not change keeps its bytes, line break included (`Ⅰ FA 4A -> 87 54`);
- a mixed or broken file gets no more lines of the encoding it has fewer of, and no more lines
  that read in no encoding.

It stops the commit and says which file, which line and what changed; it never changes a commit.
People's saves and agents' writes meet it alike. It touches nothing outside the clone (git does not
push `.git/hooks`):

- A pre-commit already there is kept (renamed `pre-commit.before-mixenc`) and runs first; the gate
  looks only when it passed. `mixenc hooks uninstall` puts it back.
- With `core.hooksPath` set (the hooks are managed elsewhere), nothing is installed; the doctor
  says so.
- `git commit --no-verify` skips the gate when a person means the change.

## VS Code (`mixenc workspace` and the extension)

The `.code-workspace` is yours: `mixenc workspace <file> --config <settings>...` adds mixenc's items
to it and keeps everything else (other folders, settings, comments). Adding the same again changes
nothing; `--dry-run` prints the changes and writes nothing. Its items:

- the repositories as folders, when they are not there; the mirrors are not (documents, a vault and
  any other folders are yours to add);
- `mixenc.configs`: the settings files this window uses (relative to the `.code-workspace`);
- vscode-clangd started through the entry (`clangd.path`; `clangd.arguments` keeps your arguments
  and gets `--mixenc-config` for each settings file), and Microsoft's C/C++ IntelliSense off, so
  that one analyzer answers;
- mixed and broken files read-only (`files.readonlyInclude`, only the entries under mixenc's
  roots): an editor reads a file in one encoding, and a save would break its other lines;
- vscode-clangd and mixenc's extension as recommended extensions.

The same settings file can serve several `.code-workspace` files. A file written whole by an earlier
mixenc loses its first comment line (it said mixenc wrote all of it). It prints how to start Copilot
CLI with the window's other folders at hand (`--add-dir`).

The extension (`editors/vscode`) is mixenc's face in VS Code; mixenc does the work. Usually nothing
shows:

- Each file opens in the encoding mixenc reads it in (a CP932 file as Shift JIS), or as the override
  table says. A file open in another encoding is opened again (VS Code 1.100's encoding API).
- Mixed, broken, ambiguous and unmirrored files are in the Problems panel with the reason; the
  read-only list follows the judgements.
- The mirror follows at start, after a save, when the window gets the focus back and after a
  checkout, merge or pull.
- A status bar item shows only when something is broken (click it) or while a long sync runs.
- A window with several settings files (`mixenc.configs`) has a session for each; they share the
  status bar item, and each registers its settings file.
- Commands: Add a repository to this window (a new settings file with the mirror outside git, or
  one there already; the changes to the `.code-workspace` are shown before they are written),
  Diagnose (the doctor's report; files that read two ways show both readings side by side, and a
  button writes the choice into the settings file), Diagnose deeply, Mirror everything again, Open
  the settings file.

Its tests run it in a real VS Code on a sample (`npm run test:vscode`).

## The deep doctor (`mixenc doctor --deep`)

By hand, and slow: every translation unit of the mirror databases goes through `clangd --check`
(half the processors at once; `--jobs`). The first run takes a baseline
(`.mixenc/deep-baseline.json` in the repository's mirror); later runs report:

- translation units with more errors than at the baseline: MSVC builds the code, so a new error
  means a file read wrong or a generated file missing (`--update-baseline` when the change is meant);
- include files not found (a `.tlh` not built yet, a folder missing from the database);
- headers no translation unit includes (clangd reads them with flags it guesses);
- how many translation units clangd's background index holds.

Progress goes to stderr (`progress 3/120 src/a.cpp` with `--json`). Like the short report, it stays
on the PC where it is made.

## Copilot's gate (`mixenc copilot`)

VS Code's Copilot and Copilot CLI run the same hooks (`~/.copilot/hooks/`). Their own tools read a
CP932 file as UTF-8 and write it back as UTF-8; the hooks put them right:

| Copilot's tool | A file that is not UTF-8 | A UTF-8 or ASCII file | A mirror |
|---|---|---|---|
| read (`view`) | reads the mirror instead (same lines) | as it is | as it is |
| search (`rg`) | the answer gets the matches read right, named by the original | as it is | — |
| write (`edit`, `create`, `apply_patch`) | denied, with the way to mixenc's `edit` | non-ASCII text where MSVC would misread it is denied; the rest goes through | denied |
| after a write or a shell command | — | the mirror is synced | — |

A session in a repository with a settings file starts with how to read and write there and the
doctor's short report. Elsewhere the hooks say nothing. A hook that times out lets the tool through: the commit
gate is the last line.

`mixenc copilot install` (into `COPILOT_HOME`, else `~/.copilot`; `--home` for another folder):

- `hooks/mixenc.json`: the hooks, running this mixenc (`exec`, no shell).
- `agents/mixenc-analyst.agent.md`: the analysis agent.
- `mcp-config.json`: the MCP server `mixenc` (other servers are kept).
- `lsp-config.json`: Copilot CLI's clangd through the entry (`--no-lsp` leaves it); the entry
  before is kept for `uninstall`.

`mixenc copilot uninstall` takes all of it out and puts clangd back; `mixenc copilot status` (and
the doctor) shows what is there. Copilot reads these when it starts. A preToolUse hook that cannot
run denies the tool, so uninstall before you remove mixenc. VS Code's Copilot reads the hooks and
the agent from the same folder; add the MCP server there as above.

Measured with Copilot CLI 1.0.105 on a made-up CP932 sample: `view` showed `"表示"` from the mirror
where it had shown `"�\��"`, `rg` found the lines it had missed, `apply_patch` was denied and the
agent turned to mixenc's `edit`, which wrote one CP932 line and kept the rest; the analysis agent
answered from a symbol to its callers and callees with the originals' paths.

## Commands

| Command | What it does |
|---|---|
| `mixenc init --repo <dir>... [--mirror <dir>]` | Write a settings file (`mixenc.json` here, or the one `--config` names) and register it when a repository is outside its folder |
| `mixenc register [<file>...]` | Register settings files, so that their repositories find them |
| `mixenc sync [--repo <name>] [--rebuild]` | Mirror the repositories and their outside include folders, write the mirror databases (`--rebuild`: every file anew) |
| `mixenc doctor [--limit N]` | Check everything, change nothing |
| `mixenc doctor --deep [--jobs N] [--update-baseline]` | The deep checks with clangd, against the baseline |
| `mixenc workspace [<file>] [--dry-run]` | Add mixenc's items to a `.code-workspace` (the settings files: `--config`, repeated) |
| `mixenc where [<path>]` | The settings file that holds a folder or file (exit code 1 when none does) |
| `mixenc files [--all]` | The files that are not plain UTF-8 or ASCII, with judgement and state (what the extension reads) |
| `mixenc override <file> <encoding\|skip>` / `--remove` | Decide how a file is read (the override table) and mirror it so at once |
| `mixenc read <file> [--as E] [--start N] [--end M]` | A file's text, read as judged or as another encoding (to compare readings) |
| `mixenc judge <file>...` | Print the judgement of files |
| `mixenc clangd [args]...` | The clangd entry (every argument goes on to clangd) |
| `mixenc edit <file> --old <text> --new <text> [--encoding E]` | The front gate: replace text, keeping the file's encoding (`--create` writes a new file) |
| `mixenc mcp` | The analysis tools and the front gate's tools as an MCP server on stdin and stdout |
| `mixenc hooks install\|uninstall\|status [--repo <name>]` | The commit gate in each repository |
| `mixenc hooks pre-commit` | Check the staged files (what the pre-commit runs) |
| `mixenc copilot install\|uninstall\|status [--home <dir>]` | Copilot's hooks, the analysis agent, the MCP server and the entry under `~/.copilot` |
| `mixenc copilot intro [--ja]` | The text that introduces the analysis agent to an orchestrator |
| `mixenc copilot hook <event>` | Answer one hook event (Copilot runs this) |

Common options: `-C <dir>` (as `git -C`), `--json`, `--config <file>` (the settings file, instead
of the one found from the folder: a settings file in it or above it whose repository or mirror holds
the folder, else a registered one, else the only one in the nearest folder that has any). Exit codes: `0` fine, `1` something to do
(`sync`: a file could not be mirrored; `doctor`: something is broken; `hooks`: not installed
because of `core.hooksPath`; `hooks pre-commit`: the commit is stopped; `copilot status`: something
is not installed), `2` an error. `copilot hook` always exits `0`.

## `mixenc.json`

A settings file is `mixenc.json` or `<name>.mixenc.json`; its paths are relative to its folder or
absolute.

```json
{
  "format_version": 1,
  "mirror": "D:/mirror",
  "repos": [
    { "path": "C:/src/cloneA" }
  ],
  "external": ["C:/Vendor/SDK/include"],
  "overrides": {
    "C:/src/cloneA/src/old.h": { "as": "cp932", "judged": "ambiguous-cp932-euc-jp" },
    "C:/src/cloneA/data/table.bin": { "as": "skip" }
  }
}
```

- `mirror`: where the mirrors go: each repository's (`<mirror>/<name>`), the outside include
  folders' (`_ext`) and mixenc's state (`.mixenc`). Default: `mirror` beside the settings file;
  refused when that is in a git work tree. Several settings files may name the same folder.
- `repos[].path`: the repository. Optional: `name` (default: the folder's name), `mirror` (its
  mirror, default `<mirror>/<name>`) and `db` (its `compile_commands.json`, the file or its folder;
  default: lazymsbuild's, else the repository's root). Documents (a Wiki, a vault) are folders of
  the `.code-workspace`, not of the settings file; an older file's `wiki` is ignored.
- `external`: include folders outside the repositories to mirror besides those the databases name.
- `overrides`: what a person decided for a file the judgement could not (or got wrong): an encoding
  to read it as, or `skip`. `judged` records the judgement at the time; the doctor lists the
  override once it changes.

mixenc refuses a mirror inside a repository, inside a git work tree, inside another mirror or over
its own folders. A mirror remembers its original: a folder that is another original's mirror is
not written (not even with `--rebuild`), and a mirror never removes another mirror inside it.

A repository outside its settings file's folder finds the file through the register
(`%LOCALAPPDATA%\mixenc\workspaces.json`, or `MIXENC_HOME`): `mixenc init`, `mixenc workspace`
and the extension register the files they use; `mixenc register` does it by hand (after a clone
of the vault, say). The doctor says when a settings file is not registered.

## Judgements

| Judgement | Mirrored | Note |
|---|---|---|
| `ascii`, `utf8`, `utf8-bom`, `cp932` | yes | |
| `utf16le`, `utf16be` | yes | with a BOM |
| `utf16le-nobom`, `utf16be-nobom` | yes | listed: MSVC needs a BOM |
| `euc-jp`, `iso-2022-jp` | yes | listed |
| `mixed` | yes, line by line | UTF-8 and CP932 lines in one file; listed (an editor misreads it) |
| `broken` | yes, U+FFFD for what reads in no encoding | listed |
| `ambiguous-utf8-cp932` | yes, as UTF-8 | listed; valid UTF-8 cannot hold the `0x5C` trap |
| `ambiguous-cp932-euc-jp` | no | waits for `overrides` |
| `binary` | no | NUL bytes that are not UTF-16 |

The order and the rules are in [`src/judge.rs`](src/judge.rs).

## What `sync` does

1. Reads the database and sees which folders and files it names.
2. Mirrors the repository: git's files (tracked, and untracked but not ignored), and generated
   files the database names (sources, forced includes, headers right in its include folders).
3. Mirrors the include folders outside the repository that are not the toolchain's.
4. Writes the mirror database: paths moved into the mirrors, response files written out,
   `/source-charset` and `/execution-charset` taken out, then checked (no path leads to an
   original).
5. Writes `.mixenc/repo.json` in the repository's mirror (the roots and clangd's `--path-mappings`), and
   judges the toolchain's headers (they should be UTF-8).

Mirror files are written through a temporary file and renamed into place, and left read-only.

`--path-mappings` lists every root twice, with an upper-case and a lower-case drive letter: clangd
matches the drive letter in its case, VS Code sends `c:`, the database has `C:`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option.
