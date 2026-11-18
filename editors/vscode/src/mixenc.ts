// Running the mixenc executable: the extension only starts it and shows what it says.

import { spawn } from 'node:child_process';

export interface Run {
  code: number;
  stdout: string;
  stderr: string;
}

export interface RunOptions {
  /** Each line mixenc writes on stderr, as it comes. */
  onStderrLine?: (line: string) => void;
  /** Set to stop the run (the process is killed). */
  signal?: AbortSignal;
}

export class Mixenc {
  constructor(
    readonly exe: string,
    readonly cwd: string,
    private readonly log: (message: string) => void,
    /** The settings file each run is about (`--config`). */
    readonly config?: string,
  ) {}

  run(args: string[], options: RunOptions = {}): Promise<Run> {
    if (this.config) {
      args = [...args, '--config', this.config];
    }
    this.log(`mixenc ${args.join(' ')}`);
    return new Promise((resolve, reject) => {
      const child = spawn(this.exe, args, { cwd: this.cwd, windowsHide: true, signal: options.signal });
      const out: Buffer[] = [];
      const err: Buffer[] = [];
      let pending = '';
      child.stdout.on('data', (b: Buffer) => out.push(b));
      child.stderr.on('data', (b: Buffer) => {
        err.push(b);
        if (options.onStderrLine) {
          pending += b.toString('utf8');
          const lines = pending.split(/\r?\n/);
          pending = lines.pop() ?? '';
          lines.forEach((l) => options.onStderrLine!(l));
        }
      });
      child.on('error', (e) => reject(e));
      child.on('close', (code) => {
        if (pending && options.onStderrLine) {
          options.onStderrLine(pending);
        }
        const result = {
          code: code ?? 1,
          stdout: Buffer.concat(out).toString('utf8'),
          stderr: Buffer.concat(err).toString('utf8'),
        };
        if (result.code >= 2) {
          this.log(`exit ${result.code}: ${result.stderr.trim()}`);
        }
        resolve(result);
      });
    });
  }

  /** Runs with `--json` and reads what it prints (exit code 1 means "something to do", not failure). */
  async json<T>(args: string[], options: RunOptions = {}): Promise<T> {
    const r = await this.run([...args, '--json'], options);
    if (r.code >= 2) {
      let message = r.stderr.trim();
      try {
        message = (JSON.parse(r.stdout) as { error?: string }).error ?? message;
      } catch {
        // not JSON: keep stderr
      }
      throw new Error(message || `mixenc ${args.join(' ')} failed (${r.code})`);
    }
    return JSON.parse(r.stdout) as T;
  }
}
