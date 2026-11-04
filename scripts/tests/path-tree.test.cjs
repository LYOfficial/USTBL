const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

const root = path.join(__dirname, "../../");

function load(file) {
  const exports = {};
  const module = { exports };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(fs.readFileSync(path.join(root, file), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS },
    }).outputText
  )(exports, module, () => ({}));
  return module.exports;
}

const {
  compareDirsFirst,
  createPathTree,
  placePath,
  sortPathTree,
  toTreeNodes,
} = load("src/utils/path-tree.ts");
const style = load("src/utils/modpack-change-style.ts");

const hooks = {
  makeDir: (p, name) => ({ path: p, name, kind: "dir", children: [] }),
  makeLeaf: (p, name) => ({ path: p, name, kind: "file", children: [] }),
};

test("placePath builds the directories above a path and merges them by lowercase", () => {
  const tree = createPathTree();
  placePath(tree, "Mods/a.jar", hooks);
  placePath(tree, "mods/b.jar", hooks);
  assert.equal(tree.roots.length, 1);
  assert.equal(tree.roots[0].name, "Mods");
  assert.deepEqual(
    tree.roots[0].children.map((node) => node.name),
    ["a.jar", "b.jar"]
  );
});

test("placePath reports directories and the leaf", () => {
  const tree = createPathTree();
  const dirs = [];
  const leaves = [];
  const options = {
    ...hooks,
    onDir: (node) => dirs.push(node.path),
    onLeaf: (node, created) => leaves.push([node.path, created]),
  };
  placePath(tree, "a/b/c.txt", options);
  placePath(tree, "a/b", options);
  assert.deepEqual(dirs, ["a", "a/b", "a"]);
  assert.deepEqual(leaves, [
    ["a/b/c.txt", true],
    ["a/b", false],
  ]);
});

test("placePath keeps two leaves with one key apart when merging is off", () => {
  const tree = createPathTree();
  placePath(tree, "a/X.txt", { ...hooks, mergeLeaves: false });
  placePath(tree, "a/x.txt", { ...hooks, mergeLeaves: false });
  assert.equal(tree.roots[0].children.length, 2);
});

test("placePath can merge by exact name", () => {
  const tree = createPathTree();
  const keyOf = (p) => p;
  placePath(tree, "Mods/a", { ...hooks, keyOf });
  placePath(tree, "mods/a", { ...hooks, keyOf });
  assert.equal(tree.roots.length, 2);
});

test("sortPathTree puts directories first and sorts every level", () => {
  const tree = createPathTree();
  for (const p of ["z.txt", "b/y.txt", "b/a.txt", "a/x.txt", "c.txt"]) {
    placePath(tree, p, hooks);
  }
  sortPathTree(tree.roots, compareDirsFirst);
  assert.deepEqual(
    tree.roots.map((node) => node.name),
    ["a", "b", "c.txt", "z.txt"]
  );
  assert.deepEqual(
    tree.roots[1].children.map((node) => node.name),
    ["a.txt", "y.txt"]
  );
});

test("toTreeNodes wraps every level with an id", () => {
  const tree = createPathTree();
  placePath(tree, "a/b.txt", hooks);
  const nodes = toTreeNodes(tree.roots, (node) => node.path.toUpperCase());
  assert.equal(nodes[0].id, "A");
  assert.equal(nodes[0].children[0].id, "A/B.TXT");
  assert.equal(nodes[0].children[0].data.name, "b.txt");
});

test("every kind of change has a color and a label that exist in each locale", () => {
  const kinds = Object.keys(style.CHANGE_COLOR);
  assert.deepEqual(kinds.sort(), [
    "added",
    "addedExcluded",
    "deleted",
    "disable",
    "enable",
    "modified",
    "onceChanged",
  ]);
  for (const locale of ["en", "fr", "ja", "zh-Hans", "zh-Hant"]) {
    const json = JSON.parse(
      fs.readFileSync(path.join(root, `src/locales/${locale}.json`), "utf8")
    );
    for (const kind of kinds) {
      const key = style.changeLabelKey(kind).split(".");
      let value = json;
      for (const part of key) value = value?.[part];
      assert.equal(typeof value, "string", `${locale} ${key.join(".")}`);
    }
    assert.equal(typeof json.FilePanel.expandAll, "string", locale);
    assert.equal(typeof json.FilePanel.collapseAll, "string", locale);
  }
});
