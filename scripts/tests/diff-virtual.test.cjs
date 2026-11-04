const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function read(file) {
  return fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
}

function load(file) {
  const module = { exports: {} };
  new Function(
    "exports",
    "module",
    ts.transpileModule(read(file), {
      compilerOptions: { module: ts.ModuleKind.CommonJS },
    }).outputText
  )(module.exports, module);
  return module.exports;
}

const diff = load("src/utils/diff-virtual.ts");

const hunk = (lines) => ({ lines: Array.from({ length: lines }, () => ({})) });

test("diff rows count lines and, when shown, one header per hunk", () => {
  const hunks = [hunk(3), hunk(4)];
  assert.equal(diff.diffRowCount(hunks, false), 9);
  assert.equal(diff.diffRowCount(hunks, true), 7);
  assert.equal(diff.diffRowCount([], false), 0);
});

test("a diff is virtualized only above the threshold", () => {
  assert.equal(diff.DIFF_VIRTUAL_THRESHOLD, 200);
  assert.equal(diff.shouldVirtualizeDiff(200), false);
  assert.equal(diff.shouldVirtualizeDiff(201), true);
  assert.equal(diff.shouldVirtualizeDiff(52390), true);
});

test("headers count toward the threshold unless hidden", () => {
  const hunks = [hunk(199), hunk(1)];
  assert.equal(
    diff.shouldVirtualizeDiff(diff.diffRowCount(hunks, false)),
    true
  );
  assert.equal(
    diff.shouldVirtualizeDiff(diff.diffRowCount(hunks, true)),
    false
  );
});

test("the visible range covers the viewport plus overscan and stays in bounds", () => {
  const rows = diff.DIFF_VIEWPORT_HEIGHT / diff.DIFF_ROW_HEIGHT;
  const top = diff.diffVisibleRange(0, 52390);
  assert.equal(top.start, 0);
  assert.ok(top.end >= rows + diff.DIFF_OVERSCAN);
  assert.ok(top.end - top.start < 60);
  const middle = diff.diffVisibleRange(1000, 52390);
  assert.equal(middle.start, 1000 - diff.DIFF_OVERSCAN);
  assert.ok(middle.end >= 1000 + rows);
  const bottom = diff.diffVisibleRange(52389, 52390);
  assert.equal(bottom.end, 52390);
  assert.ok(bottom.start < bottom.end);
});

test("text columns err wide for tabs and non-Latin text", () => {
  assert.equal(diff.textColumns(""), 0);
  assert.equal(diff.textColumns("abc"), 3);
  assert.equal(diff.textColumns("\tx"), 9);
  assert.equal(diff.textColumns("中文"), 4);
});

test("DiffHunks picks the virtual path by row count and keeps the edit action", () => {
  const source = read("src/components/common/diff-view.tsx");
  assert.match(source, /shouldVirtualizeDiff\(rowCount\)/);
  assert.match(source, /<VirtualDiffHunks/);
  assert.match(
    source,
    /action=\{lineAction\(hunk, index, renderEditAction\)\}/
  );
  assert.match(
    source,
    /action=\{lineAction\(hunk, lineIndex, renderEditAction\)\}/
  );
});
