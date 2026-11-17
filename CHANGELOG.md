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

### Fixed

- `--path-mappings` lists every root with an upper-case and a lower-case drive letter (clangd
  matches the case; VS Code sends `c:`).
- A client that read one document misread and opens another right: the columns of the one read
  right are no longer moved.
- Paths written with 8.3 short names (`RUNNER~1`) and paths in another case than the disk's are
  recognized as the same folders.
