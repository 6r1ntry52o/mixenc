// The mixenc extension: the face of the mixenc executable in VS Code. mixenc does the work (judges,
// mirrors, checks); the extension passes it the moments (opening, saving, focus, git) and shows
// what it says. Usually nothing shows: a status bar item appears only when something is broken or
// a long sync runs, and files that need care are in the Problems panel.

import * as fs from 'node:fs';
import * as path from 'node:path';
import * as vscode from 'vscode';

import {
  type FileInfo,
  type Report,
  editorEncoding,
  html,
  needsChoice,
  nonAsciiLines,
  parseProgress,
  pathKey,
  problemOf,
  readonlyInclude,
} from './logic';
import { Mixenc } from './mixenc';

const t = vscode.l10n.t;

let session: Session | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<Api> {
  const log = vscode.window.createOutputChannel('mixenc', { log: true });
  context.subscriptions.push(log);
  const command = (id: string, run: () => unknown) =>
    context.subscriptions.push(
      vscode.commands.registerCommand(id, async () => {
        try {
          await run();
        } catch (e) {
          log.error(String(e));
          void vscode.window.showErrorMessage(t('mixenc: {0}', e instanceof Error ? e.message : String(e)));
        }
      }),
    );
  command('mixenc.createWorkspace', () => createWorkspace(log));
  command('mixenc.diagnose', () => needSession().diagnose(false));
  command('mixenc.diagnoseDeep', () => needSession().diagnose(true));
  command('mixenc.rebuild', () => needSession().sync(true));
  command('mixenc.openConfig', () =>
    vscode.window.showTextDocument(vscode.Uri.file(path.join(needSession().root, 'mixenc.json'))),
  );

  const root = findWorkspace();
  if (root) {
    session = new Session(root, exePath(), log);
    context.subscriptions.push(session);
    void session.start();
  }
  return { session: () => session };
}

export function deactivate(): void {
  session = undefined;
}

/** What the integration tests look at. */
export interface Api {
  session(): Session | undefined;
}

function needSession(): Session {
  if (!session) {
    throw new Error(t('this window has no mixenc workspace (mixenc.json); "mixenc: Create a workspace" makes one'));
  }
  return session;
}

function exePath(): string {
  return vscode.workspace.getConfiguration('mixenc').get<string>('path') || 'mixenc';
}

/** The folder with mixenc.json: the setting, else the folders open and those above them. */
function findWorkspace(): string | undefined {
  const configured = vscode.workspace.getConfiguration('mixenc').get<string>('workspace');
  if (configured && fs.existsSync(path.join(configured, 'mixenc.json'))) {
    return configured;
  }
  for (const folder of vscode.workspace.workspaceFolders ?? []) {
    let dir = folder.uri.fsPath;
    for (;;) {
      if (fs.existsSync(path.join(dir, 'mixenc.json'))) {
        return dir;
      }
      const up = path.dirname(dir);
      if (up === dir) {
        break;
      }
      dir = up;
    }
  }
  return undefined;
}

export class Session implements vscode.Disposable {
  readonly mixenc: Mixenc;
  private readonly status = vscode.window.createStatusBarItem('mixenc.status', vscode.StatusBarAlignment.Left, 0);
  private readonly problems = vscode.languages.createDiagnosticCollection('mixenc');
  private readonly disposables: vscode.Disposable[] = [];
  /** Files that are not plain UTF-8, by path key. */
  files = new Map<string, FileInfo>();
  report: Report | undefined;
  private broken = 0;
  private syncing: Promise<void> | undefined;
  private again = false;
  private timer: NodeJS.Timeout | undefined;
  private lastSync = 0;
  /** Documents being opened again in another encoding (not to do it twice). */
  private reopened = new Map<string, string>();
  private page: vscode.WebviewPanel | undefined;
  /** Resolves when the first sync and refresh are done. */
  ready: Promise<void> = Promise.resolve();

  constructor(
    readonly root: string,
    exe: string,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.mixenc = new Mixenc(exe, root, (m) => log.info(m));
    this.status.name = 'mixenc';
    this.status.command = 'mixenc.diagnose';
  }

  async start(): Promise<void> {
    this.disposables.push(
      vscode.workspace.onDidOpenTextDocument((d) => void this.ensureEncoding(d)),
      vscode.workspace.onDidSaveTextDocument((d) => {
        if (this.inWorkspace(d.uri.fsPath)) {
          this.schedule(1500);
        }
      }),
      vscode.window.onDidChangeWindowState((s) => {
        if (s.focused && Date.now() - this.lastSync > 30_000) {
          this.schedule(500);
        }
      }),
    );
    const watch = (base: string, glob: string, then: () => void) => {
      const w = vscode.workspace.createFileSystemWatcher(new vscode.RelativePattern(vscode.Uri.file(base), glob));
      w.onDidChange(then);
      w.onDidCreate(then);
      w.onDidDelete(then);
      this.disposables.push(w);
    };
    watch(this.root, 'mixenc.json', () => this.schedule(500));
    for (const repo of this.repos()) {
      // A checkout, merge or pull changes the index; mirror after it.
      watch(repo, '.git/{HEAD,index}', () => this.schedule(2000));
    }
    this.ready = this.sync(false);
    await this.ready;
  }

  dispose(): void {
    clearTimeout(this.timer);
    this.disposables.forEach((d) => d.dispose());
    this.status.dispose();
    this.problems.dispose();
    this.page?.dispose();
  }

  private repos(): string[] {
    try {
      const config = JSON.parse(fs.readFileSync(path.join(this.root, 'mixenc.json'), 'utf8')) as {
        repos?: Array<{ path: string }>;
      };
      return (config.repos ?? []).map((r) => path.resolve(this.root, r.path));
    } catch {
      return [];
    }
  }

  private inWorkspace(file: string): boolean {
    const key = pathKey(file);
    return [this.root, ...this.repos()].some((r) => key.startsWith(pathKey(r) + '\\'));
  }

  private schedule(delay: number): void {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => void this.sync(false), delay);
  }

  /** Mirrors what changed (`rebuild`: everything), then shows how things stand. */
  async sync(rebuild: boolean): Promise<void> {
    if (this.syncing) {
      this.again = true;
      return this.syncing;
    }
    this.syncing = (async () => {
      // Only a sync that takes a while shows itself.
      const slow = setTimeout(() => this.showProgress(), rebuild ? 0 : 2000);
      try {
        const r = await this.mixenc.run(rebuild ? ['sync', '--rebuild', '--json'] : ['sync', '--json']);
        if (r.code >= 2) {
          this.log.error(r.stderr.trim());
        }
        this.lastSync = Date.now();
      } finally {
        clearTimeout(slow);
      }
      await this.refresh();
    })();
    try {
      await this.syncing;
    } finally {
      this.syncing = undefined;
    }
    if (this.again) {
      this.again = false;
      await this.sync(false);
    }
  }

  private showProgress(): void {
    this.status.text = `$(sync~spin) mixenc`;
    this.status.tooltip = t('mixenc is mirroring the repositories');
    this.status.show();
  }

  /** Reads the doctor's report and the file list, and shows them. */
  async refresh(): Promise<void> {
    const [report, files] = await Promise.all([
      this.mixenc.json<Report>(['doctor']).catch((e) => {
        this.log.error(`doctor: ${e}`);
        return undefined;
      }),
      this.mixenc.json<{ files: FileInfo[] }>(['files']).catch((e) => {
        this.log.error(`files: ${e}`);
        return { files: [] as FileInfo[] };
      }),
    ]);
    this.report = report;
    this.files = new Map(files.files.map((f) => [pathKey(f.path), f]));
    this.broken = report?.summary.broken ?? 0;
    if (this.broken > 0) {
      this.status.text = `$(warning) mixenc ${this.broken}`;
      this.status.tooltip = t('mixenc: {0} things are broken. Click to see them.', this.broken);
      this.status.show();
    } else {
      this.status.hide();
    }
    this.showProblems(files.files);
    await this.updateReadonly(files.files);
    for (const d of vscode.workspace.textDocuments) {
      await this.ensureEncoding(d);
    }
  }

  private showProblems(files: FileInfo[]): void {
    this.problems.clear();
    for (const f of files) {
      const p = problemOf(f);
      if (!p) {
        continue;
      }
      const message = {
        mixed: t(
          'UTF-8 and CP932 lines are mixed in this file. It is read-only here: saving it in one encoding would break the other lines. mixenc reads each line right for clangd and agents.',
        ),
        broken: t(
          'Some bytes of this file read in no encoding (shown as replacement characters). It is read-only here, so that saving does not lose them.',
        ),
        ambiguous: t(
          'This file reads both as UTF-8 and as CP932; mixenc reads it as UTF-8. If that is wrong, choose in "mixenc: Diagnose".',
        ),
        undecided: t(
          'This file reads both as CP932 and as EUC-JP. clangd does not see it until you choose in "mixenc: Diagnose".',
        ),
        'not-mirrored': t('mixenc could not mirror this file, so clangd does not see it: {0}', p.detail ?? ''),
        'utf16-nobom': t('UTF-16 without a byte order mark: MSVC reads it wrong without one.'),
        'euc-jp': t('EUC-JP: MSVC does not read it as such; mixenc reads it right for clangd.'),
        'iso-2022-jp': t('ISO-2022-JP: VS Code cannot open it in this encoding; read it with `mixenc read`.'),
      }[p.kind];
      const d = new vscode.Diagnostic(
        new vscode.Range(0, 0, 0, 0),
        message,
        p.warning ? vscode.DiagnosticSeverity.Warning : vscode.DiagnosticSeverity.Information,
      );
      d.source = 'mixenc';
      d.code = p.kind;
      this.problems.set(vscode.Uri.file(f.path), [d]);
    }
  }

  /** Mixed and broken files read-only, in the .code-workspace (never in a repository's settings). */
  private async updateReadonly(files: FileInfo[]): Promise<void> {
    const file = vscode.workspace.workspaceFile;
    if (!file || file.scheme !== 'file') {
      return;
    }
    const config = vscode.workspace.getConfiguration('files');
    const current = config.inspect<Record<string, boolean>>('readonlyInclude')?.workspaceValue;
    const next = readonlyInclude(current, this.root, files);
    if (next) {
      await config.update('readonlyInclude', next, vscode.ConfigurationTarget.Workspace);
    }
  }

  /** Opens a file again in its encoding when VS Code opened it in another. */
  async ensureEncoding(doc: vscode.TextDocument): Promise<void> {
    if (doc.uri.scheme !== 'file') {
      return;
    }
    const info = this.files.get(pathKey(doc.uri.fsPath));
    if (!info) {
      return;
    }
    const wanted = editorEncoding(info);
    const key = pathKey(doc.uri.fsPath);
    if (!wanted || doc.encoding === wanted || this.reopened.get(key) === wanted) {
      return;
    }
    if (doc.isDirty) {
      this.log.warn(`${doc.uri.fsPath}: open in ${doc.encoding}, is ${wanted}; not reopened (unsaved changes)`);
      return;
    }
    this.reopened.set(key, wanted);
    this.log.info(`${doc.uri.fsPath}: reopened in ${wanted} (was ${doc.encoding})`);
    await vscode.workspace.openTextDocument(doc.uri, { encoding: wanted });
  }

  /** The diagnose page: the doctor's report, and the files whose reading a person chooses. */
  async diagnose(deep: boolean): Promise<void> {
    let report: Report;
    if (deep) {
      const done = await vscode.window.withProgress(
        { location: vscode.ProgressLocation.Notification, title: t('mixenc: deep diagnosis'), cancellable: true },
        async (progress, token) => {
          const abort = new AbortController();
          token.onCancellationRequested(() => abort.abort());
          let last = 0;
          try {
            return await this.mixenc.json<Report>(['doctor', '--deep'], {
              signal: abort.signal,
              onStderrLine: (line) => {
                const p = parseProgress(line);
                if (p) {
                  const now = (p.done / p.total) * 100;
                  progress.report({ message: `${p.done}/${p.total} ${p.file}`, increment: now - last });
                  last = now;
                }
              },
            });
          } catch (e) {
            if (token.isCancellationRequested) {
              return undefined;
            }
            throw e;
          }
        },
      );
      if (!done) {
        return;
      }
      report = done;
    } else {
      await this.refresh();
      if (!this.report) {
        throw new Error(t('mixenc doctor gave no report (see the output "mixenc")'));
      }
      report = this.report;
    }
    await this.showPage(report, deep);
  }

  private async showPage(report: Report, deep: boolean): Promise<void> {
    if (!this.page) {
      this.page = vscode.window.createWebviewPanel('mixenc.diagnose', 'mixenc', vscode.ViewColumn.Active, {
        enableScripts: true,
      });
      this.page.onDidDispose(() => (this.page = undefined));
      this.page.webview.onDidReceiveMessage((m: { type: string; path: string; read?: string }) => {
        void this.onPageMessage(m, deep);
      });
    }
    const choices: string[] = [];
    for (const f of this.files.values()) {
      const readings = needsChoice(f);
      if (!readings) {
        continue;
      }
      const columns = await Promise.all(
        readings.map((enc) =>
          this.mixenc
            .json<{ lines: string[]; start: number }>(['read', f.path, '--as', enc])
            .then((r) => nonAsciiLines(r.lines, r.start, 12))
            .catch(() => [] as Array<[number, string]>),
        ),
      );
      const rows = (columns[0] ?? [])
        .map(([n], i) => `<tr><td>${n}</td>${columns.map((c) => `<td>${html(c[i]?.[1] ?? '')}</td>`).join('')}</tr>`)
        .join('');
      choices.push(`<section><h3>${html(f.path)}</h3>
        <table><tr><th></th>${readings.map((r) => `<th>${html(r)} <button data-path="${html(f.path)}" data-read="${html(r)}">${html(t('Read as {0}', r))}</button></th>`).join('')}</tr>${rows}</table></section>`);
    }
    const severity = { broken: t('Broken'), notice: t('Notice'), ok: t('OK') } as const;
    const items = (['broken', 'notice', 'ok'] as const)
      .map((s) => {
        const list = report.items.filter((i) => i.severity === s);
        if (!list.length) {
          return '';
        }
        return `<h2 class="${s}">${html(severity[s])} (${list.length})</h2>${list
          .map(
            (i) => `<div class="item"><div>${html(i.repo ? `${i.repo}: ` : '')}${html(i.message)}</div>
              ${i.fix ? `<div class="fix">${html(t('What to do: {0}', i.fix))}</div>` : ''}
              ${(i.paths ?? [])
                .slice(0, 30)
                .map((p) => `<div class="path"><a href="#" data-open="${html(p)}">${html(p)}</a></div>`)
                .join('')}
              ${(i.paths?.length ?? 0) > 30 ? `<div class="path">${html(t('... and {0} more', (i.paths?.length ?? 0) - 30))}</div>` : ''}</div>`,
          )
          .join('')}`;
      })
      .join('');
    const nonce = Math.random().toString(36).slice(2);
    this.page.title = deep ? t('mixenc: deep diagnosis') : t('mixenc: diagnosis');
    this.page.webview.html = `<!DOCTYPE html><html><head><meta charset="utf-8">
      <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';">
      <style>body{font-family:var(--vscode-font-family);color:var(--vscode-foreground)} .broken{color:var(--vscode-errorForeground)}
      .item{margin:.5em 0 .8em} .fix{opacity:.8;margin-left:1em} .path{margin-left:1em;font-family:var(--vscode-editor-font-family)}
      table{border-collapse:collapse} td,th{border:1px solid var(--vscode-panel-border);padding:2px 6px;text-align:left;font-family:var(--vscode-editor-font-family)}</style>
      </head><body><h1>${html(report.workspace)}</h1>
      <p>${html(t('{0} broken, {1} notices, {2} ok', report.summary.broken, report.summary.notice, report.summary.ok))}</p>
      ${items}
      ${choices.length ? `<h2>${html(t('Files whose reading you choose'))}</h2><p>${html(t('Each reading of the lines with non-ASCII text. The choice goes into mixenc.json (overrides).'))}</p>${choices.join('')}` : ''}
      <script nonce="${nonce}">const vscode = acquireVsCodeApi();
      document.addEventListener('click', (e) => { const a = e.target.closest('[data-open]'); if (a) { e.preventDefault(); vscode.postMessage({ type: 'open', path: a.dataset.open }); }
        const b = e.target.closest('button[data-read]'); if (b) { vscode.postMessage({ type: 'override', path: b.dataset.path, read: b.dataset.read }); } });</script>
      </body></html>`;
    this.page.reveal();
  }

  private async onPageMessage(m: { type: string; path: string; read?: string }, deep: boolean): Promise<void> {
    try {
      if (m.type === 'open') {
        // Paths in the report are relative to the workspace, maybe with a note after them.
        const p = m.path.replace(/ \(.*\)$/, '');
        await vscode.window.showTextDocument(vscode.Uri.file(path.resolve(this.root, p)));
      } else if (m.type === 'override' && m.read) {
        const r = await this.mixenc.run(['override', m.path, m.read]);
        if (r.code !== 0) {
          throw new Error(r.stderr.trim());
        }
        await this.sync(false);
        if (!deep && this.report) {
          await this.showPage(this.report, false);
        }
      }
    } catch (e) {
      void vscode.window.showErrorMessage(t('mixenc: {0}', e instanceof Error ? e.message : String(e)));
    }
  }
}

/** "Create a workspace": mixenc.json for the repositories (and documents), the mirror, the .code-workspace. */
async function createWorkspace(log: vscode.LogOutputChannel): Promise<void> {
  const pick = (title: string, many: boolean, defaultUri?: vscode.Uri) =>
    vscode.window.showOpenDialog({ title, canSelectFolders: true, canSelectFiles: false, canSelectMany: many, defaultUri });
  const root = (await pick(t('The workspace folder (mixenc.json and the mirror go here)'), false))?.[0];
  if (!root) {
    return;
  }
  const mixenc = new Mixenc(exePath(), root.fsPath, (m) => log.info(m));
  if (!fs.existsSync(path.join(root.fsPath, 'mixenc.json'))) {
    const repos = await pick(t('The repositories (their originals stay where they are)'), true, root);
    if (!repos?.length) {
      return;
    }
    const wiki = await pick(t('The documents (Wiki) to open beside them; cancel for none'), false, root);
    const args = ['init', ...repos.flatMap((r) => ['--repo', r.fsPath]), ...(wiki?.[0] ? ['--wiki', wiki[0].fsPath] : [])];
    const r = await mixenc.run(args);
    if (r.code !== 0) {
      throw new Error(r.stderr.trim());
    }
  }
  await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: t('mixenc: making the mirror (the first time takes a while)') },
    async () => {
      const r = await mixenc.run(['sync']);
      if (r.code >= 2) {
        throw new Error(r.stderr.trim());
      }
    },
  );
  const written = await mixenc.json<{ path: string }>(['workspace']);
  await vscode.commands.executeCommand('vscode.openFolder', vscode.Uri.file(written.path));
}
