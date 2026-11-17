# mixenc

[English](README.md) | 日本語

Shift_JIS（CP932）・UTF-8 などの文字コードがファイルごと・行ごとに混ざった C/C++ のリポを、clangd と AI エージェントが読み違えず、取りこぼさずに読めるようにします。

clangd はソースを UTF-8 として読みます。CP932 では「表」「能」「ソ」などの 2 バイト目が `0x5C`（ASCII の `\`）なので、これで終わるコメントは UTF-8 で読むと行の継続になり、次の行（宣言のことが多い）が消えます。

```cpp
// 表示する設定の表        <- 95 5C で終わる: UTF-8 で読むと "\"
int hidden(int);          <- コメントに飲まれる。clangd:「宣言が無い」
```

mixenc はリポの**写し**を UTF-8 でリポの外に作ります（行と改行は原本と同じ）。あわせて**写しの DB**（写しを指す `compile_commands.json`）も作ります。clangd は写しを読み（`--compile-commands-dir`）、`--path-mappings` が答えのパスを原本に戻します。エディタとエージェントは、元の文字コードのまま原本で作業を続けられます。

- **ファイルごとに判定は 1 つ。** 対象は次のとおりです。
  - BOM、BOM の有無を問わない UTF-16、ISO-2022-JP
  - CP932（MSVC と同じく 2 バイトを 1 組で読む）、UTF-8、EUC-JP
  - UTF-8 と CP932 の行の混在、壊れたバイト

  2 つの読み方が両方成り立つファイルは、推測せず一覧に出します。
- **書く前に確かめる。** 写しは厳密な UTF-8 で、原本と行が 1 対 1 でなければなりません。行末の `\` は、原本の行末が本物の `\` の時だけ許します。満たさないファイルは写さず一覧に出すので、clangd が間違った写しを読むことはありません。
- **注意ではなく機械で守る。** `mixenc doctor` は、何も変えずに次を確かめます。
  - 写しが index どおりか（手で変えられていない・原本より古くない）
  - 判定
  - 写しの DB（原本へのパスも文字コードの引数も残っていない）
  - ツールチェーンのヘッダ
  - 長いパス
- **長いパス。** 約 950 文字のパスでも動きます。clangd が索引を保存できない長い名前（235 文字以上）は、doctor が一覧に出します。
- **何も外に出さない。** テレメトリも通信もありません。doctor の出力にはパスが入るので、作った PC の中に置いておきます。

**状態: 5 段のうち段 3。** この版でできるのは、判定・写す・写しの DB・doctor の短い版・入口（`mixenc clangd`）・正門（`mixenc edit` と、原本を元の文字コードで書く MCP の道具）・commit の関所（git の pre-commit）です。これから作るものは次のとおりです。
- 解析の道具と解析エージェント
- Copilot のフック
- VS Code 拡張

## 要るもの

- Windows 10 か 11（CP932 は Windows の表で読みます）
- git
- リポの `compile_commands.json`。Visual Studio のソリューションなら [lazymsbuild](https://github.com/6r1ntry52o/lazymsbuild) が保ちます。mixenc は `lazymsbuild path` でその場所を見つけます
- 結果を使う clangd

## 入れ方

今はソースから入れます。`cargo install --git https://github.com/6r1ntry52o/mixenc`（Rust 1.89 以上）を実行してください。exe 付きのリリースは後で出します。

## 始め方

**ワークスペース**は `mixenc.json` を置くフォルダです。写しはその `mirror` フォルダに作ります。

```text
workspace/
├─ repos/cloneA/            リポ（原本。人もエージェントもここを直す）
├─ mirror/cloneA/           写し・写しの DB・clangd の索引
├─ mirror/_ext/<名前>/      リポの外の include の写し
└─ mixenc.json
```

1. ワークスペースで `mixenc init --repo repos/cloneA` を実行すると、`mixenc.json` ができます。
2. `mixenc sync` でリポを写し、`mirror/cloneA/compile_commands.json` を書きます。
3. `mixenc doctor` で、すべきことの一覧が出ます（崩れがあれば終了コード 1）。
4. エディタやエージェントの clangd を、入口 `mixenc clangd` に向けます（下の節）。入口が起動のたびと動いている間に写すので、手で `mixenc sync` を実行するのは入口を使わない時だけです。
5. エージェントに正門の MCP の道具を渡し、commit の関所を入れます（`mixenc hooks install`）。

## 入口（`mixenc clangd`）

`mixenc clangd [clangd の引数]` は、写しで clangd を起こし、clangd とクライアントの間に立ちます。

- クライアントが開いたフォルダ（とその上）の `mixenc.json` からワークスペースを見つけて写し、clangd を `--compile-commands-dir`（リポが 1 つの時）と `--path-mappings` 付きで起こします。ワークスペースが無ければ、全部をそのまま clangd に渡します。
- ファイルを開く・保存する前に、そのファイルを写します。リポと DB を見張り、変更が落ち着いたら写します（checkout や別の道具での変更）。
- CP932 のファイルを UTF-8 として読むクライアント（エージェントの LSP はそう読む）は、0x5C の罠ごと本文を送ってきます。その本文は写しの本文に差し替えます。それ以降、そのクライアントとの間では、列をクライアントの読み方と写しの読み方の間で読み替えます（行は同じ）。
- 写せないファイルは `window/showMessage` で知らせ、解析は続けます。入口が自分であるメッセージに失敗した時は、そのメッセージをそのまま渡し、そのことを知らせます。

起こす clangd は、次の順に探します。
1. `MIXENC_CLANGD`
2. `mixenc.json` の `"clangd"`
3. `PATH` の `clangd.exe`
4. vscode-clangd が入れたもの

入口は `mirror\.mixenc\entry.log` にログを残します（この PC の中だけ）。中身は次のとおりです。
- それぞれの本文がどう届いたか（正しく読んだ・読み違えたので差し替えた）
- 診断の件数
- 定義と参照がどこに返ったか（写しのパスが出たら不具合）

`files.encoding` を指定しない VS Code も、Copilot CLI も、CP932 のファイルを UTF-8 として読み違えて送ってきます（実測）。入口はそれを差し替えます。

- **VS Code**（vscode-clangd）：`"clangd.path": "C:\\Tools\\mixenc.exe"` と `"clangd.arguments": ["clangd", "--background-index"]`。
- **Copilot CLI**：`~/.copilot/lsp-config.json` に `"command": "mixenc"` と `"args": ["clangd", "--background-index"]`。
- **`clangd` を起こすもの全般**：`mixenc.exe` を `clangd.exe` という名前で、本物より `PATH` の前に置くと入口になります（clangd を探す時は自分を飛ばします）。

## 正門（`mixenc edit`・`mixenc mcp`）

CP932 のファイルを Unicode を通して読み書きするエディタやエージェントは、頼まれていないバイトまで変えます。`Ⅰ`・`№`・`㈱` には NEC と IBM の 2 つの形があり、違う文字コードで読んだファイルは壊れて戻ります。正門は、エージェントが原本を書くための道です。ファイルの文字を置き換え、そのファイルの文字コードのまま書き戻します。

- 符号化するのは文字が変わった行だけです（CP932 は Visual Studio と同じ形）。ほかの行はバイトも改行もそのままです。混在のファイルは、変えた行はその行の文字コード、足した行はファイルの多い方の文字コードで書きます。
- 非 ASCII がまだ無いファイルは、周りのファイルに倣います。倣う先は DB の同じプロジェクト、無ければ同じフォルダ、無ければリポ全体です。その中の `cp932`・`utf8-bom`・`utf8` で一番多いものにします（同数は `cp932`）。BOM の無い UTF-8 は、プロジェクトが `/utf-8` でビルドする時だけです。それが一番多いのに `/utf-8` が無い時は、人に決めてもらうよう返します。
- その文字コードに無い字（CP932 の `😀` など）は止めて、字と行を返します。JIS と CP932 で違う字（`〜` など）は、CP932 の字も添えます。
- 写しのパスは原本を指します。置き換える文字はファイルに 1 か所だけある必要があります。改行は `\n` で渡せます。
- 書く前に、結果が頼まれた文字として読み戻せること・commit の関所の確かめを通ること・読んだ時から原本が変わっていないことを確かめます。書いた後に写しを合わせます。

`mixenc mcp` は、これを VS Code の Copilot と Copilot CLI 向けの MCP の道具 2 つとして出します。`edit`（path・old_string・new_string）と `create`（path・content）です。

- **VS Code**：`mcp.json`（MCP: Open User Configuration）に `"servers": { "mixenc": { "type": "stdio", "command": "C:\\Tools\\mixenc.exe", "args": ["mcp"] } }`。
- **Copilot CLI**：`copilot mcp add mixenc -- mixenc mcp`。

コマンドからは `mixenc edit <file> --old <文字> --new <文字>`（`--old-file`・`--new-file`・`--create` もある）で同じことができます。

## commit の関所（`mixenc hooks`）

`mixenc hooks install` で、ワークスペースの各リポに git の pre-commit を入れます。ステージしたファイルごとに、HEAD と比べて次を確かめます。

- 文字コードが変わっていない（`cp932` が `utf8` になっていない）。非 ASCII がまだ無かったファイルは、そこで MSVC が正しく読む形だけ通す（上と同じ）
- 文字が変わっていない行は、改行まで含めてバイトが同じ（`Ⅰ FA 4A -> 87 54` を止める）
- 混在・壊れのファイルで、少ない方の文字コードの行と、どの文字コードでも読めない行が増えていない

止める時は、ファイル・行・何が変わったかを出します。commit を書き換えることはしません。人の保存も AI の書き込みも同じに止まります。clone の外には何も触りません（git は `.git/hooks` を push しない）。

- 既にある pre-commit は残し（`pre-commit.before-mixenc` に名前を変える）、先に呼びます。それが通った時だけ関所が見ます。`mixenc hooks uninstall` で元に戻ります。
- `core.hooksPath` が設定されている時（フックをほかで管理している）は入れません。doctor がそのことを出します。
- 人が意図した変更は `git commit --no-verify` で通せます。

## コマンド

| コマンド | すること |
|---|---|
| `mixenc init --repo <dir>... [--wiki <dir>]` | 今のフォルダに `mixenc.json` を書く |
| `mixenc sync [--repo <名前>]` | リポとリポの外の include を写し、写しの DB を書く |
| `mixenc doctor [--limit N]` | 全部を確かめる（何も変えない） |
| `mixenc judge <file>...` | ファイルの判定を出す |
| `mixenc clangd [args]...` | 入口（引数は全部 clangd に渡す） |
| `mixenc edit <file> --old <文字> --new <文字> [--encoding E]` | 正門：文字コードを保って置き換える（`--create` で新しいファイル） |
| `mixenc mcp` | 正門の道具を MCP サーバとして出す（標準入出力） |
| `mixenc hooks install\|uninstall\|status [--repo <名前>]` | 各リポの commit の関所 |
| `mixenc hooks pre-commit` | ステージしたファイルを確かめる（pre-commit が呼ぶ） |

共通のオプションは `-C <dir>`（`git -C` と同じ）と `--json` です。終了コードは次のとおりです。
- `0`：問題なし
- `1`：すべきことがある（`sync` は写せないファイルがあった時、`doctor` は崩れがある時、`hooks` は `core.hooksPath` で入れなかった時、`hooks pre-commit` は commit を止めた時）
- `2`：エラー

## `mixenc.json`

形は [README.md](README.md#mixencjson) を見てください。

- `repos[].path` はワークスペースからの相対パスか絶対パスです。次の 3 つは省けます。
  - `name`：写しのフォルダ名（既定はフォルダの名前）
  - `wiki`：ドキュメント
  - `db`：`compile_commands.json` のファイルかそのフォルダ（既定は lazymsbuild のもの、無ければリポの根）
- `external` には、DB が指す場所のほかに写す、リポの外の include フォルダを書きます。
- `overrides` は上書き表です。判定が決められなかった（または間違えた）ファイルについて、人が決めたことを書きます。読む文字コードか `skip` を指定し、`judged` にその時の判定を残します。判定が変わると doctor が一覧に出します。

## 判定

| 判定 | 写すか | 補足 |
|---|---|---|
| `ascii`・`utf8`・`utf8-bom`・`cp932` | 写す | |
| `utf16le`・`utf16be` | 写す | BOM あり |
| `utf16le-nobom`・`utf16be-nobom` | 写す | 一覧に出す（MSVC は BOM が要る） |
| `euc-jp`・`iso-2022-jp` | 写す | 一覧に出す |
| `mixed` | 行ごとに写す | UTF-8 と CP932 の行が 1 つのファイルにある。一覧に出す（エディタが読み違える） |
| `broken` | 写す（どの文字コードでも読めない所は U+FFFD） | 一覧に出す |
| `ambiguous-utf8-cp932` | UTF-8 で写す | 一覧に出す。厳密な UTF-8 には `0x5C` の罠が入り得ない |
| `ambiguous-cp932-euc-jp` | 写さない | 上書き表で決まるまで待つ |
| `binary` | 写さない | UTF-16 でない NUL を含む |

順番と決まりは [`src/judge.rs`](src/judge.rs) にあります。

## `--path-mappings` の注意

`--path-mappings` は根ごとに、ドライブ文字が大文字の組と小文字の組の 2 つを並べます。clangd はドライブ文字の大小を区別し、VS Code は `c:`、DB は `C:` を使うためです。大文字の組だけだと、エディタで開いたファイルが写しに替わらず、隣のヘッダの原本を読みます（clangd 23 で実測）。

## ライセンス

[Apache License 2.0](LICENSE-APACHE) か [MIT](LICENSE-MIT) のどちらかを選べます。
