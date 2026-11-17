# Third-party software

## Used at run time, not distributed with mixenc

| Software | License | How mixenc uses it |
|---|---|---|
| [clangd](https://clangd.llvm.org/) (LLVM) | Apache-2.0 WITH LLVM-exception | The entry (`mixenc clangd`), the analysis tools and `doctor --deep` start the clangd you have. |
| [git](https://git-scm.com/) | GPL-2.0 | Lists a repository's files and reads the staged ones (`git ls-files`, `git diff`, `git cat-file`). |
| [lazymsbuild](https://github.com/6r1ntry52o/lazymsbuild) | MIT OR Apache-2.0 | Where the repository's `compile_commands.json` is (`lazymsbuild path`), when it is installed. |
| VS Code, [vscode-clangd](https://github.com/clangd/vscode-clangd), GitHub Copilot and Copilot CLI | their own terms | The extension runs in VS Code; the workspace mixenc writes points vscode-clangd at the entry; `mixenc copilot install` writes Copilot's hook, agent, MCP and LSP settings. |

## Behaviour reproduced, no code copied

| Software | License | What |
|---|---|---|
| [ripgrep](https://github.com/BurntSushi/ripgrep) | Unlicense OR MIT | The form of its output (`path:line:text`, context lines, counts), which Copilot's grep answers in and mixenc completes. |

## Compiled into mixenc.exe

The Rust crates in `Cargo.lock`, unmodified; their licenses are in `THIRD_PARTY_LICENSES.html` in
each release archive (made with [cargo-about](https://github.com/EmbarkStudios/cargo-about) from
`about.toml` and `about.hbs`). CP932 is read and written with Windows' own conversion functions.

## The VS Code extension

It has no run-time dependencies; TypeScript, the VS Code API typings, `@vscode/test-electron` and
`@vscode/vsce` are used only to build, test and package it.
