# mixenc for VS Code

The face of [mixenc](https://github.com/6r1ntry52o/mixenc) in VS Code: for a C/C++ repository whose
files mix Shift_JIS (CP932), UTF-8 and other encodings. mixenc (the executable) does the work; this
extension passes it the moments and shows what it says.

Usually nothing shows.

- **Each file opens in its encoding.** A CP932 file opens as Shift JIS, a UTF-16 one as UTF-16, as
  mixenc judged them (or as the override table in `mixenc.json` says).
- **Mixed and broken files are read-only** (an editor reads a file in one encoding; saving would
  break the other lines) and are in the Problems panel with the reason.
- **The mirror follows**: at start, after a save, when the window gets the focus back, after a
  checkout, merge or pull. clangd (through the entry `mixenc clangd`) reads the mirror.
- **A status bar item appears only when something is broken** (`mixenc ⚠ 3`; click it), or while
  a long sync runs.

Commands (`mixenc:` in the command palette):

| Command | What it does |
|---|---|
| Create a workspace | Pick the workspace folder, the repositories and their documents: writes `mixenc.json`, mirrors, writes and opens the `.code-workspace` |
| Diagnose | The doctor's report: broken, notices, ok, each with what to do. Files that read both as CP932 and as EUC-JP (or as UTF-8) show both readings side by side; a button writes the choice into `mixenc.json` |
| Diagnose deeply | Every translation unit through `clangd --check`, held against the baseline the first run took (with progress; it takes a while) |
| Mirror everything again | Reads every file again and writes every mirror anew |
| Open mixenc.json | The settings and the override table |

## Requirements

- Windows, VS Code 1.100 or later
- mixenc (`mixenc.exe`): on `PATH`, or the workspace `mixenc workspace` writes names it
- [clangd](https://marketplace.visualstudio.com/items?itemName=llvm-vs-code-extensions.vscode-clangd)
  for the analysis (the workspace points it at the entry)

## Settings

- `mixenc.path`: the mixenc executable (default: `mixenc` on `PATH`). In a workspace you have not
  trusted, the workspace's value is not used.
- `mixenc.workspace`: the folder that holds `mixenc.json` (default: the open folders and those
  above them).

Nothing leaves your machine: the extension runs mixenc and reads what it prints.

License: MIT or Apache-2.0.
