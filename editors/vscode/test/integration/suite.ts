// Runs inside VS Code (see run.ts): what a person would see, checked through VS Code's API.

import assert from 'node:assert/strict';
import * as path from 'node:path';

import * as vscode from 'vscode';

import type { Api } from '../../src/extension';

async function until<T>(what: string, get: () => T | undefined | Promise<T | undefined>, ms = 30_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await get();
    if (v) {
      return v;
    }
    if (Date.now() > end) {
      throw new Error(`timed out waiting for: ${what}`);
    }
    await new Promise((r) => setTimeout(r, 200));
  }
}

export async function run(): Promise<void> {
  const root = process.env.MIXENC_TEST_ROOT!;
  const src = path.join(root, 'src', 'r1', 'src');
  const ext = vscode.extensions.getExtension<Api>('6r1ntry52o.mixenc');
  assert.ok(ext, 'the extension is there');
  const api = await ext.activate();
  // Two settings files in the vault: a session each.
  const sessions = await until('two sessions', () => (api.sessions().length === 2 ? api.sessions() : undefined));
  await Promise.all(sessions.map((s) => s.ready));
  for (const s of sessions) {
    assert.equal(s.report?.summary.broken, 0, JSON.stringify(s.report?.items));
  }
  const session = sessions[0];

  // A CP932 file opens as Shift_JIS, read right.
  const a = vscode.Uri.file(path.join(src, 'a.cpp'));
  await vscode.window.showTextDocument(a);
  const doc = await until('a.cpp in shiftjis', () =>
    vscode.workspace.textDocuments.find((d) => d.uri.fsPath === a.fsPath && d.encoding === 'shiftjis'),
  );
  assert.ok(doc.getText().includes('// 表示する'), doc.getText());
  // So does one of the other repository (the other settings file).
  const b = vscode.Uri.file(path.join(root, 'src', 'r2', 'lib', 'b.cpp'));
  await vscode.window.showTextDocument(b);
  const docB = await until('b.cpp in shiftjis', () =>
    vscode.workspace.textDocuments.find((d) => d.uri.fsPath === b.fsPath && d.encoding === 'shiftjis'),
  );
  assert.ok(docB.getText().includes('// ソース'), docB.getText());
  // The person's own line in the .code-workspace is still there.
  const own = vscode.workspace.workspaceFile!;
  assert.ok((await vscode.workspace.fs.readFile(own)).toString().includes('// my window'));

  // A mixed file is in the Problems panel and read-only.
  const m = vscode.Uri.file(path.join(src, 'm.cpp'));
  const problems = vscode.languages.getDiagnostics(m);
  assert.equal(problems[0]?.code, 'mixed', JSON.stringify(problems));
  const readonly = await until('m.cpp read-only', () => {
    const value = vscode.workspace.getConfiguration('files').get<Record<string, boolean>>('readonlyInclude') ?? {};
    return Object.keys(value).find((k) => k.toLowerCase().endsWith('/src/m.cpp'));
  });
  assert.ok(readonly);

  // The commands are there, and the diagnose page opens.
  const commands = await vscode.commands.getCommands(true);
  for (const c of ['mixenc.addRepository', 'mixenc.diagnose', 'mixenc.diagnoseDeep', 'mixenc.rebuild', 'mixenc.openConfig']) {
    assert.ok(commands.includes(c), c);
  }
  await session.diagnose(false);
  console.log(
    `mixenc suite: passed (${sessions.length} settings files, a.cpp and b.cpp opened as ${doc.encoding}/${docB.encoding}, m.cpp: ${problems[0]?.code}, read-only ${readonly})`,
  );
}
