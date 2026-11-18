// The extension in a real VS Code: a sample window made with the mixenc executable as stage 6 sets
// it up (two repositories with a settings file each in a vault kept in git, the mirrors outside it,
// the person's .code-workspace with mixenc's items added), opened in VS Code with this extension
// and nothing else. VS Code comes from
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

function sample(base: string, exe: string, env: NodeJS.ProcessEnv): string {
  const sjis = encodeSjis;
  const repo = (name: string, files: Record<string, Buffer>) => {
    const dir = path.join(base, 'src', name);
    for (const [rel, bytes] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
      fs.writeFileSync(path.join(dir, rel), bytes);
    }
    const first = path.join(dir, Object.keys(files)[0]);
    fs.writeFileSync(
      path.join(dir, 'compile_commands.json'),
      JSON.stringify([{ directory: path.dirname(first), file: first, arguments: ['cl.exe', '/c', path.basename(first)] }]),
    );
    fs.writeFileSync(path.join(dir, '.gitignore'), 'compile_commands.json\n');
    for (const args of [['init', '-q'], ['add', '-A']]) {
      execFileSync('git', args, { cwd: dir });
    }
  };
  repo('r1', {
    'src/a.cpp': sjis('// 表示する\r\nint a() { return 1; }\r\n'),
    'src/m.cpp': Buffer.concat([Buffer.from('// 能力\r\n', 'utf8'), sjis('// 表示\r\nint m;\r\n')]),
  });
  repo('r2', { 'lib/b.cpp': sjis('// ソース\r\nint b() { return 2; }\r\n') });
  const vault = path.join(base, 'vault');
  fs.mkdirSync(vault);
  execFileSync('git', ['init', '-q'], { cwd: vault });
  for (const name of ['r1', 'r2']) {
    const config = path.join(vault, `${name}.mixenc.json`);
    execFileSync(exe, ['init', '--config', config, '--repo', path.join(base, 'src', name), '--mirror', path.join(base, 'mirror')], { env });
    execFileSync(exe, ['sync', '--config', config], { env });
  }
  const codeWorkspace = path.join(vault, 'dev.code-workspace');
  fs.writeFileSync(codeWorkspace, '{\n  // my window: the vault and what mixenc adds\n  "folders": [{ "path": "." }],\n}\n');
  execFileSync(exe, ['workspace', codeWorkspace, '--config', 'r1.mixenc.json', '--config', 'r2.mixenc.json'], { cwd: vault, env });
  return codeWorkspace;
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
  const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'mixenc-vscode-')));
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'mixenc-vscode-profile-'));
  // A register of its own, so that this PC's is never written.
  const env = { ...process.env, MIXENC_HOME: path.join(profile, 'mixenc-home') };
  try {
    const codeWorkspace = sample(root, exe, env);
    await runTests({
    vscodeExecutablePath: process.env.VSCODE_EXE || undefined,
    extensionDevelopmentPath: extension,
    extensionTestsPath: path.resolve(__dirname, 'suite.js'),
    extensionTestsEnv: { MIXENC_TEST_ROOT: root, MIXENC_HOME: env.MIXENC_HOME },
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
    for (const dir of [root, profile]) {
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
    }
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
