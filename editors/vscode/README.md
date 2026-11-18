# mixenc for VS Code

The face of [mixenc](https://github.com/6r1ntry52o/mixenc) in VS Code: for a C/C++ repository whose
files mix Shift_JIS (CP932), UTF-8 and other encodings. mixenc (the executable) does the work; this
extension passes it the moments and shows what it says.

Usually nothing shows.

- **Each file opens in its encoding.** A CP932 file opens as Shift JIS, a UTF-16 one as UTF-16, as
  mixenc judged them (or as the override table in the settings file says).
- **Mixed and broken files are read-only** (an editor reads a file in one encoding; saving would
  break the other lines) and are in the Problems panel with the reason.
- **The mirror follows**: at start, after a save, when the window gets the focus back, after a
  checkout, merge or pull. clangd (through the entry `mixenc clangd`) reads the mirror.
- **A status bar item appears only when something is broken** (`mixenc ⚠ 3`; click it), or while
  a long sync runs.
- **One window, several repositories**: each settings file the window lists (`mixenc.configs`, one
  per repository is the usual) gets its own session; they share the status bar item.

Commands (`mixenc:` in the command palette):

| Command | What it does |
|---|---|
| Add a repository to this window | A new settings file (pick the repository, where its mirror goes, outside git, and where the settings file goes, such as your vault) or one there already; mirrors it; shows what it adds to the `.code-workspace` (the open one, else a new one) and writes it on your word. Your other folders, settings and comments stay |
| Diagnose | The doctor's report: broken, notices, ok, each with what to do. Files that read both as CP932 and as EUC-JP (or as UTF-8) show both readings side by side; a button writes the choice into the settings file |
| Diagnose deeply | Every translation unit through `clangd --check`, held against the baseline the first run took (with progress; it takes a while) |
| Mirror everything again | Reads every file again and writes every mirror anew |
| Open the settings file | The settings and the override table |

## Requirements

- Windows, VS Code 1.100 or later
- mixenc (`mixenc.exe`): on `PATH`, or named by the `.code-workspace` (`mixenc workspace` writes it)
- [clangd](https://marketplace.visualstudio.com/items?itemName=llvm-vs-code-extensions.vscode-clangd)
  for the analysis (the workspace points it at the entry)

## Settings

- `mixenc.path`: the mixenc executable (default: `mixenc` on `PATH`). In a workspace you have not
  trusted, the workspace's value is not used.
- `mixenc.configs`: the settings files this window uses (`mixenc.json` or `<name>.mixenc.json`,
  relative to the `.code-workspace` or absolute). `mixenc workspace` adds them. Without it: a
  `mixenc.json` in an open folder or above it. In a workspace you have not trusted, the
  workspace's value is not used.
- `mixenc.workspace` (older): the folder that holds `mixenc.json`; `mixenc.configs` replaces it.

Nothing leaves your machine: the extension runs mixenc and reads what it prints.

License: MIT or Apache-2.0.
