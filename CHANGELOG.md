# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The files mixenc writes for
others to read (`mixenc.json`, `repo.json`, the doctor's `--json` report) carry a `format_version`.

## [Unreleased]

### Added

- Stage 1: the judgement (one encoding per file), the mirror (checked line by line before it is
  written), the mirror database (paths moved, response files written out, charset options taken
  out, invariant 6 checked), the include folders outside the repository, the toolchain scan, and
  the short doctor. Commands `init`, `sync`, `doctor` and `judge`.
- Stage 2: the clangd entry, `mixenc clangd` (or mixenc.exe named `clangd.exe`): finds the
  workspace from the client's folders, syncs, starts clangd on the mirror, mirrors files as they
  are opened and saved, watches the repositories, replaces a text sent misread as UTF-8 with the
  mirror's and moves that client's columns both ways, reports what is not mirrored, and passes a
  message through when it fails on it.

- The entry's log, `mirror\.mixenc\entry.log`: how documents came, diagnostics counts, where
  definitions and references lead.
- Stage 3: the front gate, `mixenc edit` and the MCP server `mixenc mcp` (tools `edit` and
  `create`): replaces text in a file of the workspace and writes it back in its own encoding,
  encoding only the lines whose text changed; a mixed file's lines keep their encodings; a file
  with no non-ASCII text so far gets the encoding the files around it use; characters the encoding
  has no form for stop the edit with their lines; a mirror's path stands for its original; what is
  written must read back as asked and pass the commit gate's check, and the original must not have
  changed since it was read.
- Stage 3: the commit gate, `mixenc hooks install|uninstall|status|pre-commit`: a git pre-commit
  that stops a commit when a staged file's encoding changed, a line whose text did not change got
  other bytes (`Ⅰ` in NEC's form instead of IBM's, CRLF to LF), a mixed or broken file got worse,
  or non-ASCII text came into a file in a form MSVC misreads. A pre-commit already there runs
  first; with `core.hooksPath` set nothing is installed. The doctor says whether it is installed.
- Stage 4: the analysis tools in `mixenc mcp` (`find_symbol`, `definition`, `references`,
  `call_hierarchy`, `type_hierarchy`, `outline`, `read_lines`, `search`, `diagnose`): clangd's
  answers through the entry, short, with the originals' paths and the files read right; a place
  is a symbol's name (with file and line when the name stands for several); what an answer cannot
  vouch for (not in the database, still indexing, not mirrored) is said with it.
- Stage 4: the analysis agent `mixenc-analyst` (a Copilot custom agent with only those tools) and
  its introduction for an orchestrator, `mixenc copilot intro [--ja]`.
- Stage 4: Copilot's gate, `mixenc copilot hook`: reads of a file that is not UTF-8 go to its
  mirror, grep's answers get the lines it read wrong read right, built-in writes to such a file or
  to a mirror (and non-ASCII text MSVC would misread) are denied with the way to the front gate,
  the mirror follows writes and shell commands, and a session starts with how to read and write
  and the doctor's short report. `mixenc copilot install|uninstall|status` puts the hooks, the
  agent, the MCP server and the entry (as Copilot CLI's clangd) under `~/.copilot`, keeping other
  settings; the doctor says whether they are there.
- Stage 5: `mixenc workspace` writes the `.code-workspace` (repositories and documents, clangd
  through the entry, Microsoft's IntelliSense off, mixed and broken files read-only, recommended
  extensions) and prints how to start Copilot CLI with the documents.
- Stage 5: the VS Code extension (`editors/vscode`): opens each file in the encoding mixenc reads it
  in, lists files that need care in the Problems panel, keeps the read-only list and the mirror up
  to date (start, save, focus, git), shows a status bar item only when something is broken, and has
  the commands Create a workspace, Diagnose (with the two readings of an ambiguous file side by side
  and a button for the override), Diagnose deeply, Mirror everything again and Open mixenc.json.
  English and Japanese.
- Stage 5: the deep doctor, `mixenc doctor --deep`: `clangd --check` on every translation unit,
  against a baseline the first run takes; include files not found, headers no translation unit
  includes, how much clangd's index holds.
- `mixenc files`, `mixenc override`, `mixenc read` and `mixenc sync --rebuild`, for the extension
  and by hand.
- Stage 6: settings files anywhere (`mixenc.json` or `<name>.mixenc.json`, such as in a vault kept
  in git), with the mirrors where they say (`mirror`, `repos[].mirror`); a mirror in a git work
  tree, inside a repository, inside another mirror or over mixenc's folders is refused. The global
  `--config <file>`; `mixenc init --mirror`; `mixenc register`, and the doctor says when a settings
  file is not registered.
- Stage 6: `mixenc workspace <file> --config ... [--dry-run]` adds mixenc's items to the person's
  `.code-workspace` and keeps the rest (folders, settings, comments); `mixenc.configs` lists the
  window's settings files. Its help says how an agent sets up a window.
- Stage 6: the entry serves several settings files at once (`--mixenc-config <file>`, repeated):
  an editor has one clangd for all its folders. `mixenc mcp --config ...` knows them from the start.
- Stage 6: the extension runs a session per settings file of the window, with one status bar item;
  "Add a repository to this window" replaces "Create a workspace".
- Releases: a zip per architecture with `mixenc.exe` and the third-party licenses, the extension's
  `.vsix`, and `SHA256SUMS`.

### Fixed

- A mirror remembers its original: a settings file that names another original's mirror no longer
  turns it into its own (the sync removed the other's files), not even with `--rebuild`; a mirror
  never removes another mirror inside it.

- `--path-mappings` lists every root with an upper-case and a lower-case drive letter (clangd
  matches the case; VS Code sends `c:`).
- A client that read one document misread and opens another right: the columns of the one read
  right are no longer moved.
- Paths written with 8.3 short names (`RUNNER~1`) and paths in another case than the disk's are
  recognized as the same folders.
- The entry's test waits for the background index to find a definition in a file that is not
  open (clangd answers with the declaration in the open file first).
