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

**状態: 5 段とも作った。** できるのは、判定・写す・写しの DB・doctor（短い版と深い版）・入口（`mixenc clangd`）・解析の道具（MCP で clangd の答えを返す）と解析エージェント・正門（`mixenc edit` と、原本を元の文字コードで書く MCP の道具）・commit の関所（git の pre-commit）・Copilot の関所（VS Code の Copilot と Copilot CLI のフック）・VS Code 拡張とそれが開くワークスペースのファイルです。

## 要るもの

- Windows 10 か 11（CP932 は Windows の表で読みます）
- git
- リポの `compile_commands.json`。Visual Studio のソリューションなら [lazymsbuild](https://github.com/6r1ntry52o/lazymsbuild) が保ちます。mixenc は `lazymsbuild path` でその場所を見つけます
- 結果を使う clangd

## 入れ方

- **mixenc**：[Releases](https://github.com/6r1ntry52o/mixenc/releases) から PC に合う zip（`x86_64` か `aarch64`）を取り、`SHA256SUMS` で確かめ、`mixenc.exe` を `PATH` に置きます。ソースからは `cargo install --git https://github.com/6r1ntry52o/mixenc`（Rust 1.89 以上）。
- **VS Code 拡張**：同じリリースの `mixenc-vscode-<版>.vsix` を `code --install-extension mixenc-vscode-<版>.vsix` で入れます。

## 始め方

**設定ファイル**（`mixenc.json` か `<名前>.mixenc.json`）に、リポの場所とその写しの置き場を書きます。リポごとに 1 枚が普通です。設定ファイルは自分のノート（git で管理する vault でよい）に置き、写しは git の外に置きます。決まったフォルダの形はありません。

```text
C:/src/cloneA/                 リポ（原本。人もエージェントもここを直す）
D:/mirror/cloneA/              写し・写しの DB・clangd の索引
D:/mirror/_ext/<名前>/         リポの外の include の写し
D:/mirror/.mixenc/             mixenc の状態とログ
<vault>/                       git で管理する。設定と窓だけ
├─ cloneA.mixenc.json          { "mirror": "D:/mirror", "repos": [{ "path": "C:/src/cloneA" }] }
├─ cloneB.mixenc.json
└─ dev.code-workspace          自分の窓。mixenc が自分の項目（mixenc.configs・clangd など）を足す
```

1. `mixenc init --config <vault>/cloneA.mixenc.json --repo C:/src/cloneA --mirror D:/mirror` で設定ファイルを書き、登録します（リポから見つかるように）。`--config` が無ければ今のフォルダに `mixenc.json` を書きます。`--mirror` が無ければ写しはその隣の `mirror` に置きますが、そこが git の作業ツリーの中なら止めます。
2. `mixenc sync`（リポの中で、か `--config` 付きで）でリポを写し、写しの DB を書きます。
3. `mixenc doctor` で、すべきことの一覧が出ます（崩れがあれば終了コード 1）。
4. エディタやエージェントの clangd を、入口 `mixenc clangd` に向けます（下の節）。入口が起動のたびと動いている間に写すので、手で `mixenc sync` を実行するのは入口を使わない時だけです。
5. commit の関所を入れます（`mixenc hooks install`）。
6. Copilot（VS Code と Copilot CLI）には、`mixenc copilot install` でフック・解析エージェント・MCP サーバ・入口を `~/.copilot` に入れます（下の節）。
7. VS Code には、`mixenc workspace <vault>/dev.code-workspace --config <vault>/cloneA.mixenc.json --config <vault>/cloneB.mixenc.json` で、自分の `.code-workspace` に mixenc の項目を足します（無ければ作ります。`--dry-run` で先に変わる所を見られます）。拡張のコマンド「mixenc: この窓にリポを足す」なら、1・2・7 をダイアログで済ませます。

窓の用意をエージェントに頼む時も、エージェントはこのコマンドを呼びます。`.code-workspace` と設定ファイルを手で直しません（手順は `mixenc workspace --help`）。

## 入口（`mixenc clangd`）

`mixenc clangd [clangd の引数]` は、写しで clangd を起こし、clangd とクライアントの間に立ちます。

- `--mixenc-config <ファイル>` で渡された設定ファイル（繰り返せる。エディタは窓の全部のフォルダに clangd を 1 つしか起こさない）を、無ければクライアントが開いたフォルダごとの設定ファイル（そこか上にあるもの、か登録したもの）を扱います。写して、clangd を `--compile-commands-dir`（リポが 1 つの時）と `--path-mappings` 付きで起こします。写しが重なる設定ファイルは外し、そのことを知らせます。ワークスペースが無ければ、全部をそのまま clangd に渡します。
- ファイルを開く・保存する前に、そのファイルを写します。リポと DB を見張り、変更が落ち着いたら写します（checkout や別の道具での変更）。
- CP932 のファイルを UTF-8 として読むクライアント（エージェントの LSP はそう読む）は、0x5C の罠ごと本文を送ってきます。その本文は写しの本文に差し替えます。それ以降、そのクライアントとの間では、列をクライアントの読み方と写しの読み方の間で読み替えます（行は同じ）。
- 写せないファイルは `window/showMessage` で知らせ、解析は続けます。入口が自分であるメッセージに失敗した時は、そのメッセージをそのまま渡し、そのことを知らせます。

起こす clangd は、次の順に探します。
1. `MIXENC_CLANGD`
2. `mixenc.json` の `"clangd"`
3. `PATH` の `clangd.exe`
4. vscode-clangd が入れたもの
5. Neovim の Mason が入れたもの

入口は `<写しの置き場>\.mixenc\entry.log` にログを残します（この PC の中だけ）。中身は次のとおりです。
- それぞれの本文がどう届いたか（正しく読んだ・読み違えたので差し替えた）
- 診断の件数
- 定義と参照がどこに返ったか（写しのパスが出たら不具合）

`files.encoding` を指定しない VS Code も、Copilot CLI も、CP932 のファイルを UTF-8 として読み違えて送ってきます（実測）。入口はそれを差し替えます。

- **VS Code**（vscode-clangd）：`"clangd.path": "C:\\Tools\\mixenc.exe"` と `"clangd.arguments": ["clangd", "--background-index", "--mixenc-config", "<設定ファイル>", ...]`（`mixenc workspace` が両方を書きます）。
- **Copilot CLI**：`~/.copilot/lsp-config.json` に `"command": "mixenc"` と `"args": ["clangd", "--background-index"]`。
- **`clangd` を起こすもの全般**：`mixenc.exe` を `clangd.exe` という名前で、本物より `PATH` の前に置くと入口になります（clangd を探す時は自分を飛ばします）。

## 解析の道具（`mixenc mcp`）

`mixenc mcp` は MCP サーバです。解析の道具は clangd に聞き（自分で入口を起こします）、原本のパスで、どのファイルも正しく読んで、短く答えます。

| 道具 | 返すもの |
|---|---|
| `find_symbol` | 名前（の一部）で clangd の索引から見つけた記号の種類・修飾名・パス:行 |
| `definition` | 記号の定義と宣言の場所と、その前後の行 |
| `references` | 使われている件数と、ファイルごとの行（本文は付けない） |
| `call_hierarchy` | 関数を呼ぶ側（incoming）か呼ばれる側（outgoing）の木。段の深さと呼んでいる行つき |
| `type_hierarchy` | 基底クラスと派生クラスの木 |
| `outline` | ファイルの宣言と行（入れ子） |
| `read_lines` | ファイルの行を正しく読んで返す（CP932・混在・UTF-16） |
| `search` | 写しを正規表現で探し、原本のパス:行で返す |
| `diagnose` | doctor の短い版・clangd の索引の状態。ファイルを渡すと、その文字コード・DB にあるか・clangd のエラー |

場所を聞く道具には記号の名前を渡します。同じ名前の記号が複数ある時だけ、名前が書かれたファイルと行も渡します（列は数えなくてよい）。答えを保証できない時（`compile_commands.json` に無いファイル・clangd が索引を作っている途中・写していないファイル）は、「not vouched for」にそう書きます。

**解析エージェント**（`mixenc-analyst`・`mixenc copilot install` で入る）は、この道具だけを持つ Copilot のカスタムエージェントです。記号を探す → 定義・参照・呼び出し階層・型の階層 → 要る行だけ読む、の順に調べ、文字列・コメント・コンパイルされないファイルの時だけ文字を探します。答えは、結論・根拠（パス:行）・確かめていないこと、の形です。`mixenc copilot intro`（日本語は `--ja`）で、オーケストラの指示に貼る短い紹介文を出します。

## 正門（`mixenc edit`・`mixenc mcp`）

CP932 のファイルを Unicode を通して読み書きするエディタやエージェントは、頼まれていないバイトまで変えます。`Ⅰ`・`№`・`㈱` には NEC と IBM の 2 つの形があり、違う文字コードで読んだファイルは壊れて戻ります。正門は、エージェントが原本を書くための道です。ファイルの文字を置き換え、そのファイルの文字コードのまま書き戻します。

- 符号化するのは文字が変わった行だけです（CP932 は Visual Studio と同じ形）。ほかの行はバイトも改行もそのままです。混在のファイルは、変えた行はその行の文字コード、足した行はファイルの多い方の文字コードで書きます。
- 非 ASCII がまだ無いファイルは、周りのファイルに倣います。倣う先は DB の同じプロジェクト、無ければ同じフォルダ、無ければリポ全体です。その中の `cp932`・`utf8-bom`・`utf8` で一番多いものにします（同数は `cp932`）。BOM の無い UTF-8 は、プロジェクトが `/utf-8` でビルドする時だけです。それが一番多いのに `/utf-8` が無い時は、人に決めてもらうよう返します。
- その文字コードに無い字（CP932 の `😀` など）は止めて、字と行を返します。JIS と CP932 で違う字（`〜` など）は、CP932 の字も添えます。
- 写しのパスは原本を指します。置き換える文字はファイルに 1 か所だけある必要があります。改行は `\n` で渡せます。
- 書く前に、結果が頼まれた文字として読み戻せること・commit の関所の確かめを通ること・読んだ時から原本が変わっていないことを確かめます。書いた後に写しを合わせます。

`mixenc mcp` は、これも VS Code の Copilot と Copilot CLI 向けの MCP の道具 2 つとして出します。`edit`（path・old_string・new_string）と `create`（path・content）です。

- **VS Code**：`mcp.json`（MCP: Open User Configuration）に `"servers": { "mixenc": { "type": "stdio", "command": "C:\\Tools\\mixenc.exe", "args": ["mcp"] } }`。
- **Copilot CLI**：`mixenc copilot install` が登録します（または `copilot mcp add mixenc -- mixenc mcp`）。

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

## VS Code（`mixenc workspace` と拡張）

`.code-workspace` は自分のものです。`mixenc workspace <ファイル> --config <設定ファイル>...` は、そこに mixenc の項目を足し、ほか（ほかのフォルダ・設定・コメント）は残します。同じものをもう一度足しても何も変わりません。`--dry-run` は変わる所を出すだけで書きません。mixenc の項目は次のとおりです。

- リポ（とそのドキュメント `wiki`）のフォルダ（無ければ足す。写しは入れない）
- `mixenc.configs`：この窓が使う設定ファイル（`.code-workspace` からの相対）
- vscode-clangd を入口から起こす（`clangd.path`。`clangd.arguments` は自分の引数を残し、設定ファイルごとに `--mixenc-config` を足す）。Microsoft の C/C++ の IntelliSense は止め、答えるのを 1 つにする
- 混在・壊れのファイルは読み取り専用（`files.readonlyInclude`。mixenc の根の下の項目だけを直す）。エディタはファイルを 1 つの文字コードで読むので、保存するとほかの行が壊れる
- 勧める拡張に vscode-clangd と mixenc の拡張

同じ設定ファイルを、いくつもの `.code-workspace` で使えます。前の mixenc が丸ごと書いたファイルは、先頭の印の行（mixenc が全部を書くと言っていた行）を外します。窓のほかのフォルダと一緒に Copilot CLI を起こすコマンド（`--add-dir`）も出します。

拡張（`editors/vscode`）は、VS Code での mixenc の顔です。仕事は mixenc がします。いつもは何も出しません。

- ファイルは、mixenc が読む文字コード（CP932 なら Shift JIS）か上書き表のとおりに開きます。違う文字コードで開いていたら開き直します（VS Code 1.100 の文字コードの API）
- 混在・壊れ・あいまい・写せないファイルは、理由と一緒に「問題」に出します。読み取り専用の一覧は判定に合わせて直します
- 写しは、起動・保存・窓にフォーカスが戻った時・checkout や merge や pull の後に合わせます
- ステータスバーには、崩れがある時（押すと診断）と、長い照合の間だけ出します
- 設定ファイルが複数ある窓（`mixenc.configs`）では、設定ファイルごとに動き、ステータスバーは 1 つにまとめます。それぞれの設定ファイルを登録します
- コマンド: この窓にリポを足す（写しを git の外に置く新しい設定ファイルか、既にある設定ファイル。`.code-workspace` に足すものを見せてから書く）・診断（doctor の結果。2 通りに読めるファイルは読み方を並べ、ボタンで設定ファイルに書く）・深い診断・写しを作り直す・設定ファイルを開く

拡張の試験は、見本を本物の VS Code で開いて確かめます（`npm run test:vscode`）。

## doctor の深い版（`mixenc doctor --deep`）

手で動かす、時間のかかる確かめです。写しの DB の全部の翻訳単位を `clangd --check` に通します（既定でプロセッサの半分ずつ・`--jobs`）。初めての時に基準（`mirror/<リポ>/.mixenc/deep-baseline.json`）を取り、次からは次を出します。

- 基準よりエラーが増えた翻訳単位。MSVC ではビルドできるので、増えたエラーは読み違えたファイルか、まだ無い生成ファイル（意図した変更なら `--update-baseline`）
- 見つからない include（まだビルドしていない `.tlh`、DB に無いフォルダ）
- どの翻訳単位からも include されないヘッダ（clangd は推測の引数で読む）
- clangd の背景の索引に入った翻訳単位の数

進み具合は stderr に出します（`--json` の時は `progress 3/120 src/a.cpp`）。短い版と同じく、作った PC の中に置いておきます。

## Copilot の関所（`mixenc copilot`）

VS Code の Copilot と Copilot CLI は、同じフック（`~/.copilot/hooks/`）を動かします。どちらも自分の道具で CP932 のファイルを UTF-8 として読み、UTF-8 として書き戻します。フックがそれを正します。

| Copilot の道具 | UTF-8 でないファイル | UTF-8・ASCII のファイル | 写し |
|---|---|---|---|
| 読む（`view`） | 写しを読ませる（行は同じ） | そのまま | そのまま |
| 探す（`rg`） | 正しく読んだ一致を、原本のパスで答えに入れる | そのまま | — |
| 書く（`edit`・`create`・`apply_patch`） | 止めて、mixenc の `edit` の使い方を返す | MSVC が読み違える所への非 ASCII は止める。ほかは通す | 止める |
| 書いた後・シェルの後 | — | 写しを合わせる | — |

ワークスペースでの会話は、読み方と書き方の案内と doctor の短い版から始まります。ワークスペースの外ではフックは何もしません。フックが時間切れになると道具は通ります。最後の守りは commit の関所です。

`mixenc copilot install`（`COPILOT_HOME`、無ければ `~/.copilot` に入れる。別のフォルダは `--home`）で入るもの:

- `hooks/mixenc.json`：フック。この mixenc を直接起こします（`exec`・シェルを通さない）
- `agents/mixenc-analyst.agent.md`：解析エージェント
- `mcp-config.json`：MCP サーバ `mixenc`（ほかのサーバは残す）
- `lsp-config.json`：Copilot CLI の clangd を入口に向ける（`--no-lsp` で変えない）。前の設定は `uninstall` のために残す

`mixenc copilot uninstall` で全部を外し、clangd の設定を元に戻します。`mixenc copilot status`（と doctor）で入っているものが分かります。Copilot はこれらを起動の時に読みます。preToolUse のフックが起こせないと道具は止められるので、mixenc を消す前に uninstall してください。VS Code の Copilot も同じフォルダのフックとエージェントを読みます。MCP サーバは上の方法で足します。

作った CP932 の見本で Copilot CLI 1.0.105 を使った実測: `view` は `"�\��"` と出していた所を写しから `"表示"` と出し、`rg` は取りこぼしていた行を見つけ、`apply_patch` は止められて、エージェントは mixenc の `edit` に切り替え、CP932 の 1 行だけを書いてほかを保ちました。解析エージェントは、記号から呼ぶ側・呼ばれる側まで原本のパスで答えました。

## コマンド

| コマンド | すること |
|---|---|
| `mixenc init --repo <dir>... [--mirror <dir>] [--wiki <dir>]` | 設定ファイルを書く（今のフォルダの `mixenc.json` か `--config` のファイル）。リポがそのフォルダの外なら登録する |
| `mixenc register [<file>...]` | 設定ファイルを登録する（リポから見つかるように） |
| `mixenc sync [--repo <名前>] [--rebuild]` | リポとリポの外の include を写し、写しの DB を書く（`--rebuild` は全部を作り直す） |
| `mixenc doctor [--limit N]` | 全部を確かめる（何も変えない） |
| `mixenc doctor --deep [--jobs N] [--update-baseline]` | clangd で深く確かめ、基準と比べる |
| `mixenc workspace [<file>] [--dry-run]` | `.code-workspace` に mixenc の項目を足す（設定ファイルは `--config` を繰り返す） |
| `mixenc where [<path>]` | フォルダやファイルを持つ設定ファイル（無ければ終了コード 1） |
| `mixenc files [--all]` | UTF-8・ASCII でないファイルと、その判定と状態（拡張が読む） |
| `mixenc override <file> <文字コード\|skip>` / `--remove` | ファイルの読み方を決め（上書き表）、すぐその読み方で写す |
| `mixenc read <file> [--as E] [--start N] [--end M]` | ファイルの本文を、判定どおりか別の文字コードで読んで出す（読み方を比べる） |
| `mixenc judge <file>...` | ファイルの判定を出す |
| `mixenc clangd [args]...` | 入口（引数は全部 clangd に渡す） |
| `mixenc edit <file> --old <文字> --new <文字> [--encoding E]` | 正門：文字コードを保って置き換える（`--create` で新しいファイル） |
| `mixenc mcp` | 解析の道具と正門の道具を MCP サーバとして出す（標準入出力） |
| `mixenc hooks install\|uninstall\|status [--repo <名前>]` | 各リポの commit の関所 |
| `mixenc hooks pre-commit` | ステージしたファイルを確かめる（pre-commit が呼ぶ） |
| `mixenc copilot install\|uninstall\|status [--home <dir>]` | Copilot のフック・解析エージェント・MCP サーバ・入口を `~/.copilot` に |
| `mixenc copilot intro [--ja]` | 解析エージェントをオーケストラに紹介する文 |
| `mixenc copilot hook <event>` | フックの 1 回分に答える（Copilot が呼ぶ） |

共通のオプションは `-C <dir>`（`git -C` と同じ）・`--json`・`--config <file>`（設定ファイル。無ければフォルダから探す: そこか上の設定ファイルでリポか写しがそのフォルダを持つもの、次に登録したもの、次にいちばん近いフォルダにある 1 枚）です。終了コードは次のとおりです。
- `0`：問題なし
- `1`：すべきことがある（`sync` は写せないファイルがあった時、`doctor` は崩れがある時、`hooks` は `core.hooksPath` で入れなかった時、`hooks pre-commit` は commit を止めた時、`copilot status` は入っていないものがある時）
- `2`：エラー
- `copilot hook` は常に `0`

## `mixenc.json`

設定ファイルは `mixenc.json` か `<名前>.mixenc.json` です。中のパスは、そのフォルダからの相対か絶対です。形は [README.md](README.md#mixencjson) を見てください。

- `mirror`：写しの置き場。リポの写し（`<mirror>/<name>`）・リポの外の include の写し（`_ext`）・mixenc の状態（`.mixenc`）を置きます。既定はファイルの隣の `mirror` ですが、そこが git の作業ツリーの中なら止めます。いくつもの設定ファイルが同じ置き場を使えます。
- `repos[].path` はリポです。次の 4 つは省けます。
  - `name`：名前（既定はフォルダの名前）
  - `mirror`：そのリポの写し（既定は `<mirror>/<name>`）
  - `wiki`：ドキュメント
  - `db`：`compile_commands.json` のファイルかそのフォルダ（既定は lazymsbuild のもの、無ければリポの根）
- `external` には、DB が指す場所のほかに写す、リポの外の include フォルダを書きます。
- `overrides` は上書き表です。判定が決められなかった（または間違えた）ファイルについて、人が決めたことを書きます。読む文字コードか `skip` を指定し、`judged` にその時の判定を残します。判定が変わると doctor が一覧に出します。

写しをリポの中・git の作業ツリーの中・ほかの写しの中・mixenc 自身のフォルダの上に置く設定は止めます。写しはどの原本の写しかを覚えていて、ほかの原本の写しのフォルダには書きません（`--rebuild` でも）。写しが、中にあるほかの写しを消すこともありません。

設定ファイルのフォルダの外にあるリポは、登録（`%LOCALAPPDATA%\mixenc\workspaces.json`、または `MIXENC_HOME`）から設定ファイルを見つけます。`mixenc init`・`mixenc workspace`・拡張は使う設定ファイルを登録します。`mixenc register` は手で登録します（vault を clone した後など）。登録されていなければ doctor が知らせます。

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
