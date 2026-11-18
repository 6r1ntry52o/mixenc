// What the extension decides, apart from VS Code (so that it can be tested with node alone).

/** A file as `mixenc files --json` lists it. */
export interface FileInfo {
  path: string;
  repo: string;
  external: boolean;
  judged: string;
  read_as: string | null;
  mirrored: boolean;
  problem: { kind: string; detail?: string } | null;
  readonly: boolean;
  /** For a mixed or broken file: the encoding most of its lines are in. */
  major: string | null;
}

/** The doctor's report (`mixenc doctor --json`), as far as the extension reads it. */
export interface Report {
  workspace: string;
  summary: { broken: number; notice: number; ok: number; units?: number };
  items: Array<{
    severity: 'broken' | 'notice' | 'ok';
    check: string;
    repo?: string;
    message: string;
    fix?: string;
    count: number;
    paths?: string[];
  }>;
}

/** A path as a key: Windows paths compare without case and with either slash. */
export function pathKey(p: string): string {
  return p.replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
}

/** The encoding VS Code opens the file in, by VS Code's name; `undefined` when VS Code has none. */
export function editorEncoding(info: FileInfo): string | undefined {
  const read = info.read_as ?? info.judged;
  switch (read) {
    case 'ascii':
    case 'utf8':
    case 'ambiguous-utf8-cp932':
      return 'utf8';
    case 'utf8-bom':
      return 'utf8bom';
    case 'cp932':
      return 'shiftjis';
    case 'euc-jp':
      return 'eucjp';
    case 'utf16le':
    case 'utf16le-nobom':
      return 'utf16le';
    case 'utf16be':
    case 'utf16be-nobom':
      return 'utf16be';
    case 'mixed':
    case 'broken':
      return info.major === 'utf8' ? 'utf8' : 'shiftjis';
    default:
      // iso-2022-jp, binary, a judgement that waits for a person.
      return undefined;
  }
}

/** Why a file is shown in the Problems panel. */
export type Kind =
  | 'mixed'
  | 'broken'
  | 'ambiguous'
  | 'undecided'
  | 'not-mirrored'
  | 'utf16-nobom'
  | 'euc-jp'
  | 'iso-2022-jp';

export interface Problem {
  kind: Kind;
  warning: boolean;
  detail?: string;
}

/** What to say about a file in the Problems panel, if anything. */
export function problemOf(info: FileInfo): Problem | undefined {
  const p = info.problem;
  if (p && p.kind === 'undecided') {
    return { kind: 'undecided', warning: true };
  }
  if (p && p.kind !== 'skipped') {
    return { kind: 'not-mirrored', warning: true, detail: p.detail ?? p.kind };
  }
  if (info.read_as && info.read_as !== info.judged) {
    // A person decided how to read it.
    return undefined;
  }
  switch (info.judged) {
    case 'mixed':
      return { kind: 'mixed', warning: false };
    case 'broken':
      return { kind: 'broken', warning: true };
    case 'ambiguous-utf8-cp932':
      return { kind: 'ambiguous', warning: false };
    case 'utf16le-nobom':
    case 'utf16be-nobom':
      return { kind: 'utf16-nobom', warning: false };
    case 'euc-jp':
      return { kind: 'euc-jp', warning: false };
    case 'iso-2022-jp':
      return { kind: 'iso-2022-jp', warning: false };
    default:
      return undefined;
  }
}

/** Files whose encoding a person has to choose (the diagnose page offers the readings). */
export function needsChoice(info: FileInfo): string[] | undefined {
  if (info.read_as && info.read_as !== info.judged) {
    return undefined;
  }
  if (info.judged === 'ambiguous-cp932-euc-jp') {
    return ['cp932', 'euc-jp'];
  }
  if (info.judged === 'ambiguous-utf8-cp932') {
    return ['utf8', 'cp932'];
  }
  return undefined;
}

/** A path as a glob that matches only itself (as `mixenc workspace` writes them). */
export function globLiteral(p: string): string {
  return p.replace(/\\/g, '/').replace(/[[\]{}*?]/g, (c) => `[${c}]`);
}

/**
 * `files.readonlyInclude` with the read-only files of a workspace: entries mixenc wrote for files
 * under its `roots` (what it mirrors) are replaced, other entries are kept. `undefined` when
 * nothing changes.
 */
export function readonlyInclude(
  current: Record<string, boolean> | undefined,
  roots: string[],
  files: FileInfo[],
): Record<string, boolean> | undefined {
  const prefixes = roots.map((r) => globLiteral(r).toLowerCase().replace(/\/?$/, '/'));
  const next: Record<string, boolean> = {};
  for (const [glob, on] of Object.entries(current ?? {})) {
    if (!prefixes.some((p) => glob.toLowerCase().startsWith(p))) {
      next[glob] = on;
    }
  }
  for (const f of files) {
    if (f.readonly) {
      next[globLiteral(f.path)] = true;
    }
  }
  const same = (a: Record<string, boolean>, b: Record<string, boolean>) =>
    Object.keys(a).length === Object.keys(b).length && Object.entries(a).every(([k, v]) => b[k] === v);
  return same(next, current ?? {}) ? undefined : next;
}

/**
 * The settings files of the window: `mixenc.configs` (relative to the .code-workspace's folder),
 * else the older `mixenc.workspace` (the folder of a mixenc.json). Each once, by path key.
 */
export function configFiles(configs: unknown, legacyFolder: unknown, base: string): string[] {
  const out: string[] = [];
  const add = (p: string) => {
    const abs = isAbsolute(p) ? p : joinPath(base, p);
    if (!out.some((o) => pathKey(o) === pathKey(abs))) {
      out.push(abs);
    }
  };
  if (Array.isArray(configs)) {
    configs.filter((c): c is string => typeof c === 'string' && c.length > 0).forEach(add);
  }
  if (!out.length && typeof legacyFolder === 'string' && legacyFolder) {
    add(joinPath(legacyFolder, 'mixenc.json'));
  }
  return out;
}

/** Whether a Windows path is absolute (a drive and a slash, or a UNC path). */
function isAbsolute(p: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(p) || p.startsWith('\\\\') || p.startsWith('//');
}

/** `base` and a relative path, with `..` and `.` resolved, `\\` between the parts. */
function joinPath(base: string, rel: string): string {
  const parts: string[] = [];
  for (const part of `${base}\\${rel}`.split(/[\\/]+/)) {
    if (part === '..') {
      parts.pop();
    } else if (part && part !== '.') {
      parts.push(part);
    }
  }
  return (base.startsWith('\\\\') ? '\\\\' : '') + parts.join('\\');
}

/** A progress line of `mixenc doctor --deep --json` on stderr: `progress 3/10 src/a.cpp`. */
export function parseProgress(line: string): { done: number; total: number; file: string } | undefined {
  const m = /^progress (\d+)\/(\d+) (.*)$/.exec(line.trim());
  return m ? { done: Number(m[1]), total: Number(m[2]), file: m[3] } : undefined;
}

/** The lines with non-ASCII text, numbered from `start` (to set two readings side by side). */
export function nonAsciiLines(lines: string[], start: number, limit: number): Array<[number, string]> {
  const out: Array<[number, string]> = [];
  lines.forEach((l, i) => {
    if (out.length < limit && /[^\x00-\x7f]/.test(l)) {
      out.push([start + i, l]);
    }
  });
  return out;
}

/** Escapes text for the diagnose page. */
export function html(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
}
