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

### Fixed

- `--path-mappings` lists every root with an upper-case and a lower-case drive letter (clangd
  matches the case; VS Code sends `c:`).
- A client that read one document misread and opens another right: the columns of the one read
  right are no longer moved.
- Paths written with 8.3 short names (`RUNNER~1`) and paths in another case than the disk's are
  recognized as the same folders.
- The entry's test waits for the background index to find a definition in a file that is not
  open (clangd answers with the declaration in the open file first).
