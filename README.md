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

**Status: stage 2 of 5.** This version judges, mirrors, writes the mirror database, runs the
short doctor, and has the clangd entry (`mixenc clangd`). Still to come: the MCP tools (analysis,
and writing originals in their own encoding), the Copilot hooks and git pre-commit gate, and the
VS Code extension.

## Requirements

- Windows 10 or 11 (mixenc reads CP932 with Windows' own table)
- git
- A `compile_commands.json` for the repository; [lazymsbuild](https://github.com/6r1ntry52o/lazymsbuild)
  keeps one for a Visual Studio solution, and mixenc finds it through `lazymsbuild path`
- clangd, to use the result

## Install

From source, for now: `cargo install --git https://github.com/6r1ntry52o/mixenc` (Rust 1.89 or
later). Releases with an exe will follow.

## Getting started

A **workspace** is a folder that holds `mixenc.json`; the mirrors go to its `mirror` folder.

```text
workspace/
├─ repos/cloneA/            the repository (originals; people and agents edit these)
├─ mirror/cloneA/           its mirror, the mirror database, clangd's index
├─ mirror/_ext/<name>/      mirrors of include folders outside the repository
└─ mixenc.json
```

1. In the workspace: `mixenc init --repo repos/cloneA` writes `mixenc.json`.
2. `mixenc sync` mirrors the repository and writes `mirror/cloneA/compile_commands.json`.
3. `mixenc doctor` lists what needs doing (exit code 1 when something is broken).
4. Point your editor's or agent's clangd at the entry, `mixenc clangd` (below). It syncs at every
   start and while it runs, so `mixenc sync` by hand is only needed without it.

## The clangd entry

`mixenc clangd [clangd's arguments]` starts clangd on the mirror and stands between it and the
client:

- It finds the workspace from the folders the client opens (`mixenc.json` there or above), syncs,
  and starts clangd with `--compile-commands-dir` (one repository) and `--path-mappings`. Without
  a workspace it passes everything through to clangd.
- Before a file is opened or saved, it mirrors that file. It watches the repositories and the
  database, and syncs after a change has settled (a checkout, another tool).
- A client that reads CP932 files as UTF-8 (agents' LSP clients do) sends the 0x5C trap along:
  such a text is replaced with the mirror's, and from then on that client's columns are moved
  between its reading and the mirror's, both ways (the lines are the same).
- Files that are not mirrored are reported with `window/showMessage`; analysis goes on. If the
  entry itself fails on a message, it passes that message through unchanged and says so.

Which clangd it starts: `MIXENC_CLANGD`, else `"clangd"` in `mixenc.json`, else `clangd.exe` on
`PATH`, else the one vscode-clangd installed.

It keeps a log in `mirror\.mixenc\entry.log` (on this PC only): how each document came (read right,
misread and replaced), the diagnostics counts, and where definitions and references lead (a
mirror path there would be a bug). Both VS Code without a `files.encoding` and Copilot CLI send CP932
files misread as UTF-8 (measured); the entry replaces them.

- **VS Code** (vscode-clangd): `"clangd.path": "C:\\Tools\\mixenc.exe"` and
  `"clangd.arguments": ["clangd", "--background-index"]`.
- **Copilot CLI**: in `~/.copilot/lsp-config.json`, `"command": "mixenc"`,
  `"args": ["clangd", "--background-index"]`.
- **Anything that runs `clangd`**: a copy of `mixenc.exe` named `clangd.exe`, earlier on `PATH` than
  the real one, is the entry (it skips itself when it looks for clangd).

## Commands

| Command | What it does |
|---|---|
| `mixenc init --repo <dir>... [--wiki <dir>]` | Write `mixenc.json` in the current folder |
| `mixenc sync [--repo <name>]` | Mirror the repositories and their outside include folders, write the mirror databases |
| `mixenc doctor [--limit N]` | Check everything, change nothing |
| `mixenc judge <file>...` | Print the judgement of files |
| `mixenc clangd [args]...` | The clangd entry (every argument goes on to clangd) |

Common options: `-C <dir>` (as `git -C`), `--json`. Exit codes: `0` fine, `1` something to do
(`sync`: a file could not be mirrored; `doctor`: something is broken), `2` an error.

## `mixenc.json`

```json
{
  "format_version": 1,
  "repos": [
    { "path": "repos/cloneA", "wiki": "repos/cloneA_Wiki" }
  ],
  "external": ["C:/Vendor/SDK/include"],
  "overrides": {
    "repos/cloneA/src/old.h": { "as": "cp932", "judged": "ambiguous-cp932-euc-jp" },
    "repos/cloneA/data/table.bin": { "as": "skip" }
  }
}
```

- `repos[].path`: relative to the workspace or absolute. `name` (the mirror folder's name,
  default: the folder's name), `wiki` (its documents) and `db` (its `compile_commands.json`, the
  file or its folder; default: lazymsbuild's, else the repository's root) are optional.
- `external`: include folders outside the repositories to mirror besides those the databases name.
- `overrides`: what a person decided for a file the judgement could not (or got wrong): an encoding
  to read it as, or `skip`. `judged` records the judgement at the time; the doctor lists the
  override once it changes.

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
5. Writes `mirror/<name>/.mixenc/repo.json` (the roots and clangd's `--path-mappings`), and
   judges the toolchain's headers (they should be UTF-8).

Mirror files are written through a temporary file and renamed into place, and left read-only.

`--path-mappings` lists every root twice, with an upper-case and a lower-case drive letter: clangd
matches the drive letter in its case, VS Code sends `c:`, the database has `C:`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option.
