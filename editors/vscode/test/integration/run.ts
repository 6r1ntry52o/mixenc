// The extension in a real VS Code: a sample workspace (a CP932 file, a mixed one) made with the
// mixenc executable, opened in VS Code with this extension and nothing else. VS Code comes from
// VSCODE_EXE (the installed one) or is downloaded; mixenc from MIXENC_EXE or this repository's
// target folder. A profile of its own (user data, extensions) leaves the user's VS Code alone.

import { execFileSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

import { runTests } from '@vscode/test-electron';

const extension = path.resolve(__dirname, '../../..');

function mixencExe(): string {
  const candidates = [
    process.env.MIXENC_EXE,
    path.resolve(extension, '../../target/release/mixenc.exe'),
    path.resolve(extension, '../../target/debug/mixenc.exe'),
  ];
  const found = candidates.find((c) => c && fs.existsSync(c));
  if (!found) {
    throw new Error('mixenc.exe not found: build it (cargo build) or set MIXENC_EXE');
  }
  return found;
}

function sample(exe: string): { root: string; codeWorkspace: string } {
  const base = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'mixenc-vscode-')));
  const root = path.join(base, 'ws');
  const src = path.join(root, 'repos', 'r1', 'src');
  fs.mkdirSync(src, { recursive: true });
  const sjis = encodeSjis;
  fs.writeFileSync(path.join(src, 'a.cpp'), sjis('// 表示する\r\nint a() { return 1; }\r\n'));
  fs.writeFileSync(path.join(src, 'm.cpp'), Buffer.concat([Buffer.from('// 能力\r\n', 'utf8'), sjis('// 表示\r\nint m;\r\n')]));
  fs.writeFileSync(
    path.join(root, 'repos', 'r1', 'compile_commands.json'),
    JSON.stringify([{ directory: src, file: path.join(src, 'a.cpp'), arguments: ['cl.exe', '/c', 'a.cpp'] }]),
  );
  fs.writeFileSync(path.join(root, 'repos', 'r1', '.gitignore'), 'compile_commands.json\n');
  for (const args of [['init', '-q'], ['add', '-A']]) {
    execFileSync('git', args, { cwd: path.join(root, 'repos', 'r1') });
  }
  fs.writeFileSync(path.join(root, 'mixenc.json'), JSON.stringify({ repos: [{ path: 'repos/r1', db: 'repos/r1/compile_commands.json' }] }));
  execFileSync(exe, ['sync'], { cwd: root });
  const written = JSON.parse(execFileSync(exe, ['workspace', '--json'], { cwd: root }).toString('utf8')) as { path: string };
  return { root, codeWorkspace: written.path };
}

/** Shift_JIS through Windows' own table (PowerShell), as the sample's bytes. */
function encodeSjis(s: string): Buffer {
  const b64 = execFileSync('powershell', [
    '-NoProfile',
    '-Command',
    `[Convert]::ToBase64String([Text.Encoding]::GetEncoding(932).GetBytes([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${Buffer.from(s, 'utf8').toString('base64')}'))))`,
  ])
    .toString('ascii')
    .trim();
  return Buffer.from(b64, 'base64');
}

async function main(): Promise<void> {
  const exe = mixencExe();
  const { root, codeWorkspace } = sample(exe);
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'mixenc-vscode-profile-'));
  try {
    await runTests({
    vscodeExecutablePath: process.env.VSCODE_EXE || undefined,
    extensionDevelopmentPath: extension,
    extensionTestsPath: path.resolve(__dirname, 'suite.js'),
    extensionTestsEnv: { MIXENC_TEST_ROOT: root },
    launchArgs: [
      codeWorkspace,
      '--user-data-dir',
      path.join(profile, 'user'),
      '--extensions-dir',
      path.join(profile, 'extensions'),
      '--disable-workspace-trust',
      '--skip-welcome',
      '--skip-release-notes',
    ],
    });
  } finally {
    // Only what this run made: the sample and the profile.
    for (const dir of [path.dirname(root), profile]) {
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
    }
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
