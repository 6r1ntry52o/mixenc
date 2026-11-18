// The mixenc extension: the face of the mixenc executable in VS Code. mixenc does the work (judges,
// mirrors, checks); the extension passes it the moments (opening, saving, focus, git) and shows
// what it says. Usually nothing shows: a status bar item appears only when something is broken or
// a long sync runs, and files that need care are in the Problems panel. A window may use several
// settings files (`mixenc.configs`, one per repository): each gets a session, and they share the
// status bar item.

import * as fs from 'node:fs';
import * as path from 'node:path';
import * as vscode from 'vscode';

import {
  type FileInfo,
  type Report,
  configFiles,
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

let sessions: Session[] = [];
let status: vscode.StatusBarItem | undefined;
/** Writes to the .code-workspace one at a time (sessions refresh at the same moments). */
let writing: Promise<unknown> = Promise.resolve();

export async function activate(context: vscode.ExtensionContext): Promise<Api> {
  const log = vscode.window.createOutputChannel('mixenc', { log: true });
  status = vscode.window.createStatusBarItem('mixenc.status', vscode.StatusBarAlignment.Left, 0);
  status.name = 'mixenc';
  status.command = 'mixenc.diagnose';
  context.subscriptions.push(log, status, { dispose: stopSessions });
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
  command('mixenc.addRepository', () => addRepository(log));
  command('mixenc.diagnose', async () => (await pickSession(true))?.diagnose(false));
  command('mixenc.diagnoseDeep', async () => (await pickSession(false))?.diagnose(true));
  command('mixenc.rebuild', async () => Promise.all(needSessions().map((s) => s.sync(true))));
  command('mixenc.openConfig', async () => {
    const s = await pickSession(false);
    if (s) {
      await vscode.window.showTextDocument(vscode.Uri.file(s.config));
    }
  });
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('mixenc.configs') || e.affectsConfiguration('mixenc.workspace')) {
        startSessions(log);
      }
    }),
  );
  startSessions(log);
  return { session: () => sessions[0], sessions: () => sessions };
}

export function deactivate(): void {
  stopSessions();
}

/** What the integration tests look at. */
export interface Api {
  session(): Session | undefined;
  sessions(): Session[];
}

function stopSessions(): void {
  sessions.forEach((s) => s.dispose());
  sessions = [];
  updateStatus();
}

function startSessions(log: vscode.LogOutputChannel): void {
  stopSessions();
  sessions = findConfigs().map((config) => new Session(config, exePath(), log));
  log.info(`settings files: ${sessions.map((s) => s.config).join(', ') || '(none)'}`);
  for (const s of sessions) {
    void s.start();
  }
}

function needSessions(): Session[] {
  if (!sessions.length) {
    throw new Error(t('this window has no mixenc settings file; "mixenc: Add a repository to this window" makes one'));
  }
  return sessions;
}

/** The session to act on: the only one, else one the person picks (those with problems first). */
async function pickSession(brokenFirst: boolean): Promise<Session | undefined> {
  const all = needSessions();
  if (all.length === 1) {
    return all[0];
  }
  const ordered = brokenFirst ? [...all].sort((a, b) => b.brokenCount - a.brokenCount) : all;
  const picked = await vscode.window.showQuickPick(
    ordered.map((s) => ({
      label: path.basename(s.config),
      description: s.brokenCount ? t('{0} broken', s.brokenCount) : '',
      detail: s.config,
      session: s,
    })),
    { title: t('Which settings file?') },
  );
  return picked?.session;
}

/** The status bar item, over every session: a long sync, else how many things are broken. */
function updateStatus(): void {
  if (!status) {
    return;
  }
  const broken = sessions.reduce((n, s) => n + s.brokenCount, 0);
  if (sessions.some((s) => s.syncingLong)) {
    status.text = `$(sync~spin) mixenc`;
    status.tooltip = t('mixenc is mirroring the repositories');
    status.show();
  } else if (broken > 0) {
    status.text = `$(warning) mixenc ${broken}`;
    status.tooltip = t('mixenc: {0} things are broken. Click to see them.', broken);
    status.show();
  } else {
    status.hide();
  }
}

function exePath(): string {
  return vscode.workspace.getConfiguration('mixenc').get<string>('path') || 'mixenc';
}

/**
 * The window's settings files: `mixenc.configs` (relative to the .code-workspace), else the older
 * `mixenc.workspace`, else a mixenc.json in an open folder or above it.
 */
function findConfigs(): string[] {
  const config = vscode.workspace.getConfiguration('mixenc');
  const file = vscode.workspace.workspaceFile;
  const base =
    file?.scheme === 'file' ? path.dirname(file.fsPath) : (vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? '');
  const configured = configFiles(config.get('configs'), config.get('workspace'), base).filter((f) => fs.existsSync(f));
  if (configured.length) {
    return configured;
  }
  for (const folder of vscode.workspace.workspaceFolders ?? []) {
    let dir = folder.uri.fsPath;
    for (;;) {
      if (fs.existsSync(path.join(dir, 'mixenc.json'))) {
        return [path.join(dir, 'mixenc.json')];
      }
      const up = path.dirname(dir);
      if (up === dir) {
        break;
      }
      dir = up;
    }
  }
  return [];
}

export class Session implements vscode.Disposable {
  readonly mixenc: Mixenc;
  /** The folder of the settings file: what the paths in it and in the report start from. */
  readonly root: string;
  private readonly problems = vscode.languages.createDiagnosticCollection('mixenc');
  private readonly disposables: vscode.Disposable[] = [];
  /** Files that are not plain UTF-8, by path key. */
  files = new Map<string, FileInfo>();
  report: Report | undefined;
  brokenCount = 0;
  syncingLong = false;
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
    readonly config: string,
    exe: string,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.root = path.dirname(config);
    this.mixenc = new Mixenc(exe, this.root, (m) => log.info(m), config);
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
    watch(this.root, path.basename(this.config), () => this.schedule(500));
    for (const repo of this.repos()) {
      // A checkout, merge or pull changes the index; mirror after it.
      watch(repo, '.git/{HEAD,index}', () => this.schedule(2000));
    }
    // Its repositories find the settings file through the register (Copilot's hooks, the gate).
    void this.mixenc.run(['register']);
    this.ready = this.sync(false);
    await this.ready;
  }

  dispose(): void {
    clearTimeout(this.timer);
    this.disposables.forEach((d) => d.dispose());
    this.problems.dispose();
    this.page?.dispose();
  }

  private repos(): string[] {
    try {
      const config = JSON.parse(fs.readFileSync(this.config, 'utf8')) as {
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
      const slow = setTimeout(() => {
        this.syncingLong = true;
        updateStatus();
      }, rebuild ? 0 : 2000);
      try {
        const r = await this.mixenc.run(rebuild ? ['sync', '--rebuild', '--json'] : ['sync', '--json']);
        if (r.code >= 2) {
          this.log.error(r.stderr.trim());
        }
        this.lastSync = Date.now();
      } finally {
        clearTimeout(slow);
        this.syncingLong = false;
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

  /** Reads the doctor's report and the file list, and shows them. */
  async refresh(): Promise<void> {
    const [report, files] = await Promise.all([
      this.mixenc.json<Report>(['doctor']).catch((e) => {
        this.log.error(`doctor: ${e}`);
        return undefined;
      }),
      this.mixenc.json<{ files: FileInfo[]; roots?: string[] }>(['files']).catch((e) => {
        this.log.error(`files: ${e}`);
        return { files: [] as FileInfo[], roots: undefined };
      }),
    ]);
    this.report = report;
    this.files = new Map(files.files.map((f) => [pathKey(f.path), f]));
    this.brokenCount = report?.summary.broken ?? 0;
    updateStatus();
    this.showProblems(files.files);
    if (files.roots) {
      await this.updateReadonly(files.roots, files.files);
    }
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
  private async updateReadonly(roots: string[], files: FileInfo[]): Promise<void> {
    const file = vscode.workspace.workspaceFile;
    if (!file || file.scheme !== 'file') {
      return;
    }
    const update = async () => {
      const config = vscode.workspace.getConfiguration('files');
      const current = config.inspect<Record<string, boolean>>('readonlyInclude')?.workspaceValue;
      const next = readonlyInclude(current, roots, files);
      if (next) {
        await config.update('readonlyInclude', next, vscode.ConfigurationTarget.Workspace);
      }
    };
    writing = writing.then(update, update);
    await writing;
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
        // Paths in the report are relative to the settings file's folder, maybe with a note after them.
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

/**
 * "Add a repository to this window": its settings file (a new one, with the mirror outside git, or
 * one there already), its first mirror, and mixenc's items in the .code-workspace (the open one,
 * else a new one). mixenc does each step; the changes are shown before they are written.
 */
async function addRepository(log: vscode.LogOutputChannel): Promise<void> {
  const folder = async (title: string, defaultUri?: vscode.Uri) =>
    (await vscode.window.showOpenDialog({ title, canSelectFolders: true, canSelectFiles: false, defaultUri }))?.[0];
  const open = vscode.workspace.workspaceFile?.scheme === 'file' ? vscode.workspace.workspaceFile : undefined;
  const near = open ? vscode.Uri.file(path.dirname(open.fsPath)) : vscode.workspace.workspaceFolders?.[0]?.uri;
  const exe = exePath();

  const how = await vscode.window.showQuickPick(
    [
      { label: t('A new settings file'), detail: t('For a repository: where its mirror goes, and where the settings file goes (a vault in git is fine)'), make: true },
      { label: t('A settings file there already'), detail: t('A mixenc.json or <name>.mixenc.json'), make: false },
    ],
    { title: t('mixenc: Add a repository to this window') },
  );
  if (!how) {
    return;
  }
  let config: string;
  if (how.make) {
    const repo = await folder(t('The repository (its originals stay where they are)'), near);
    if (!repo) {
      return;
    }
    const mirror = await folder(t('Where the mirrors go (outside git; they can be made again)'), near);
    if (!mirror) {
      return;
    }
    const target = await vscode.window.showSaveDialog({
      title: t('The settings file (keep it beside the .code-workspace, such as in your vault)'),
      defaultUri: vscode.Uri.joinPath(near ?? mirror, `${path.basename(repo.fsPath)}.mixenc.json`),
      filters: { mixenc: ['json'] },
    });
    if (!target) {
      return;
    }
    config = target.fsPath;
    const r = await new Mixenc(exe, path.dirname(config), (m) => log.info(m)).run([
      'init',
      '--config',
      config,
      '--repo',
      repo.fsPath,
      '--mirror',
      mirror.fsPath,
    ]);
    if (r.code !== 0) {
      throw new Error(r.stderr.trim());
    }
  } else {
    const picked = await vscode.window.showOpenDialog({
      title: t('The settings file'),
      canSelectFiles: true,
      defaultUri: near,
      filters: { mixenc: ['json'] },
    });
    if (!picked?.[0]) {
      return;
    }
    config = picked[0].fsPath;
  }
  const mixenc = new Mixenc(exe, path.dirname(config), (m) => log.info(m), config);
  await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: t('mixenc: making the mirror (the first time takes a while)') },
    async () => {
      const r = await mixenc.run(['sync']);
      if (r.code >= 2) {
        throw new Error(r.stderr.trim());
      }
    },
  );

  const codeWorkspace =
    open ??
    (await vscode.window.showSaveDialog({
      title: t('The .code-workspace to open (a new one)'),
      defaultUri: vscode.Uri.file(path.join(path.dirname(config), 'dev.code-workspace')),
      filters: { 'VS Code workspace': ['code-workspace'] },
    }));
  if (!codeWorkspace) {
    return;
  }
  const dry = await mixenc.json<{ changes: string[] }>(['workspace', codeWorkspace.fsPath, '--dry-run']);
  if (dry.changes.length) {
    const go = t('Write');
    const answer = await vscode.window.showInformationMessage(
      t('mixenc adds these to {0}', path.basename(codeWorkspace.fsPath)),
      { modal: true, detail: dry.changes.join('\n') },
      go,
    );
    if (answer !== go) {
      return;
    }
    await mixenc.json(['workspace', codeWorkspace.fsPath]);
  }
  if (!open) {
    await vscode.commands.executeCommand('vscode.openFolder', codeWorkspace);
  }
}
