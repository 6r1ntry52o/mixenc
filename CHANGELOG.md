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
