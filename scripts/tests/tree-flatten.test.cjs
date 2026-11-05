const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

const root = path.join(__dirname, "../../");

function read(file) {
  return fs.readFileSync(path.join(root, file), "utf8");
}

function load(file) {
  const exports = {};
  const module = { exports };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(read(file), {
      compilerOptions: { module: ts.ModuleKind.CommonJS },
    }).outputText
  )(exports, module, () => ({}));
  return module.exports;
}

const { flattenTree, indentRem } = load("src/utils/tree-flatten.ts");

const node = (id, children) => ({ id, data: null, children });

test("flattenTree lists open branches depth first and skips closed ones", () => {
  const tree = [
    node("a", [node("a/x", [node("a/x/1")]), node("a/y")]),
    node("b", [node("b/z")]),
  ];
  const rows = flattenTree(tree, (id) => id === "a" || id === "a/x");
  assert.deepEqual(
    rows.map((row) => [row.node.id, row.depth]),
    [
      ["a", 0],
      ["a/x", 1],
      ["a/x/1", 2],
      ["a/y", 1],
      ["b", 0],
    ]
  );
  const b = rows.find((row) => row.node.id === "b");
  assert.equal(b.isExpanded, false);
  assert.equal(b.isLeaf, false);
  assert.equal(rows[0].isExpanded, true);
});

test("flattenTree reports position among siblings and ignores leaves marked open", () => {
  const rows = flattenTree([node("a"), node("b"), node("c")], () => true);
  assert.deepEqual(
    rows.map((row) => [row.posInSet, row.setSize, row.isLeaf, row.isExpanded]),
    [
      [1, 3, true, false],
      [2, 3, true, false],
      [3, 3, true, false],
    ]
  );
});

test("flattenTree does not walk a closed branch", () => {
  let asked = 0;
  const tree = [node("a", [node("a/x", [node("a/x/1")])])];
  flattenTree(tree, (id) => {
    asked += 1;
    return id === "a" && false;
  });
  assert.equal(asked, 1);
});

test("indent grows by one fixed step per level at every depth", () => {
  let previous = -1;
  for (let depth = 0; depth < 40; depth += 1) {
    const rem = indentRem(depth);
    assert.ok(rem > previous, `depth ${depth}`);
    assert.equal(rem, depth * 0.75);
    previous = rem;
  }
  assert.equal(indentRem(5, 4), 5);
});

test("the tree indents by a computed rem and draws only the rows in view", () => {
  const source = read("src/components/common/tree-view.tsx");
  assert.doesNotMatch(source, /pl=\{depth/);
  assert.match(source, /paddingLeft: `\$\{indentRem\(depth, indentSize\)\}rem`/);
  assert.match(source, /flattenTree\(nodes, isOpen\)/);
  assert.match(source, /<VirtualRows/);
  const rows = read("src/components/common/virtual-rows.tsx");
  assert.match(rows, /from "react-virtualized"/);
  assert.match(rows, /new ResizeObserver/);
});

test("no caller wraps the tree in a scroll box of its own", () => {
  for (const file of [
    "src/components/common/publish-file-tree.tsx",
    "src/components/common/modpack-file-tree.tsx",
  ]) {
    assert.doesNotMatch(read(file), /overflowY="auto"/, file);
  }
});

test("the publish tree and the local changes dialog drive the tree's expand and collapse", () => {
  const tree = read("src/components/common/tree-view.tsx");
  assert.match(tree, /branchIds\(nodes\)/);
  assert.match(tree, /applyBulkExpansion/);
  for (const file of [
    "src/components/common/publish-file-tree.tsx",
    "src/components/modals/modpack-local-changes-modal.tsx",
  ]) {
    const source = read(file);
    assert.match(source, /onExpandAll=/, file);
    assert.match(source, /onCollapseAll=/, file);
    assert.match(source, /command=\{command\}/, file);
  }
  // The dialog also opens or closes every diff card.
  const dialog = read("src/components/modals/modpack-local-changes-modal.tsx");
  assert.match(dialog, /setAllCardsOpen\(open\)/);
  assert.match(dialog, /open=\{cardOpen\(entry\.path\)\}/);
});
