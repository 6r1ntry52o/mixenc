import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  type FileInfo,
  editorEncoding,
  globLiteral,
  needsChoice,
  nonAsciiLines,
  parseProgress,
  pathKey,
  problemOf,
  readonlyInclude,
} from '../src/logic';

function file(judged: string, more: Partial<FileInfo> = {}): FileInfo {
  return {
    path: 'C:\\w\\repos\\r1\\a.cpp',
    repo: 'r1',
    external: false,
    judged,
    read_as: judged,
    mirrored: true,
    problem: null,
    readonly: false,
    major: null,
    ...more,
  };
}

test('files open in the encoding mixenc reads them in', () => {
  assert.equal(editorEncoding(file('cp932')), 'shiftjis');
  assert.equal(editorEncoding(file('utf8-bom')), 'utf8bom');
  assert.equal(editorEncoding(file('utf16le-nobom')), 'utf16le');
  assert.equal(editorEncoding(file('euc-jp')), 'eucjp');
  assert.equal(editorEncoding(file('ambiguous-utf8-cp932')), 'utf8');
  // A person's decision wins; a mixed file opens as most of its lines are.
  assert.equal(editorEncoding(file('ambiguous-cp932-euc-jp', { read_as: 'euc-jp' })), 'eucjp');
  assert.equal(editorEncoding(file('mixed', { major: 'utf8' })), 'utf8');
  assert.equal(editorEncoding(file('mixed', { major: 'cp932' })), 'shiftjis');
  // VS Code has no ISO-2022-JP; an undecided file is left as VS Code opens it.
  assert.equal(editorEncoding(file('iso-2022-jp')), undefined);
  assert.equal(editorEncoding(file('ambiguous-cp932-euc-jp', { read_as: null, mirrored: false })), undefined);
});

test('only files that need care are problems', () => {
  assert.equal(problemOf(file('cp932')), undefined);
  assert.deepEqual(problemOf(file('mixed')), { kind: 'mixed', warning: false });
  assert.deepEqual(problemOf(file('broken')), { kind: 'broken', warning: true });
  const undecided = file('ambiguous-cp932-euc-jp', { read_as: null, mirrored: false, problem: { kind: 'undecided' } });
  assert.deepEqual(problemOf(undecided), { kind: 'undecided', warning: true });
  const failed = file('cp932', { mirrored: false, problem: { kind: 'unreadable', detail: 'in use' } });
  assert.deepEqual(problemOf(failed), { kind: 'not-mirrored', warning: true, detail: 'in use' });
  assert.equal(problemOf(file('cp932', { problem: { kind: 'skipped' } })), undefined);
  // Decided by a person: no longer a problem.
  assert.equal(problemOf(file('ambiguous-utf8-cp932', { read_as: 'cp932' })), undefined);
  assert.deepEqual(needsChoice(undecided), ['cp932', 'euc-jp']);
  assert.equal(needsChoice(file('ambiguous-utf8-cp932', { read_as: 'cp932' })), undefined);
});

test('the read-only list keeps what is not mixenc\'s', () => {
  const root = 'C:\\w';
  const files = [file('mixed', { path: 'C:\\w\\repos\\r1\\m[1].cpp', readonly: true }), file('cp932')];
  const next = readonlyInclude({ 'C:/other/x.h': true, 'C:/w/repos/r1/old.cpp': true }, root, files);
  assert.deepEqual(next, { 'C:/other/x.h': true, 'C:/w/repos/r1/m[[]1[]].cpp': true });
  assert.equal(readonlyInclude(next, root, files), undefined, 'nothing to change');
  assert.equal(globLiteral('C:\\a{b}*.h'), 'C:/a[{]b[}][*].h');
});

test('small readers', () => {
  assert.equal(pathKey('C:/W/Repos/'), 'c:\\w\\repos');
  assert.deepEqual(parseProgress('progress 3/10 src/a b.cpp'), { done: 3, total: 10, file: 'src/a b.cpp' });
  assert.equal(parseProgress('[3/10] x'), undefined);
  assert.deepEqual(nonAsciiLines(['int a;', '// 表', 'x', '// 能'], 5, 1), [[6, '// 表']]);
});
