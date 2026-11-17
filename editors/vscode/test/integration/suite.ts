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
  const src = path.join(root, 'repos', 'r1', 'src');
  const ext = vscode.extensions.getExtension<Api>('6r1ntry52o.mixenc');
  assert.ok(ext, 'the extension is there');
  const api = await ext.activate();
  const session = await until('a session', () => api.session());
  await session.ready;
  assert.equal(session.report?.summary.broken, 0, JSON.stringify(session.report?.items));

  // A CP932 file opens as Shift_JIS, read right.
  const a = vscode.Uri.file(path.join(src, 'a.cpp'));
  await vscode.window.showTextDocument(a);
  const doc = await until('a.cpp in shiftjis', () =>
    vscode.workspace.textDocuments.find((d) => d.uri.fsPath === a.fsPath && d.encoding === 'shiftjis'),
  );
  assert.ok(doc.getText().includes('// 表示する'), doc.getText());

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
  for (const c of ['mixenc.createWorkspace', 'mixenc.diagnose', 'mixenc.diagnoseDeep', 'mixenc.rebuild', 'mixenc.openConfig']) {
    assert.ok(commands.includes(c), c);
  }
  await vscode.commands.executeCommand('mixenc.diagnose');
  console.log(`mixenc suite: passed (a.cpp opened as ${doc.encoding}, m.cpp: ${problems[0]?.code}, read-only ${readonly})`);
}
