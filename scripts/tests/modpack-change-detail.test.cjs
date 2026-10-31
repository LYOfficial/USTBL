const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

// Transpile the TS module on the fly; its imports are types only.
function load(file) {
  const exports = {};
  const module = { exports };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(
      fs.readFileSync(path.join(__dirname, "../../", file), "utf8"),
      { compilerOptions: { module: ts.ModuleKind.CommonJS } }
    ).outputText
  )(exports, module, () => ({}));
  return module.exports;
}

const {
  CLASS_ORDER,
  OPEN_LIMIT,
  buildChangeTree,
  countByClass,
  countedTotal,
  expandedDepth,
  filterEntries,
  revealPath,
  splitByGroup,
} = load("src/utils/modpack-change-detail.ts");

// Plan 《整合包本地改动明细》§3.3 and §5 are the source of every expectation.

const entry = (p, cls, extra = {}) => ({
  path: p,
  class: cls,
  counted: ["modified", "deleted", "toggled"].includes(cls),
  toggled: false,
  disabled: false,
  ...extra,
});

const sample = [
  entry("config/a.toml", "modified"),
  entry("config/b.toml", "deleted"),
  entry("mods/x.jar", "toggled", { disabled: true }),
  entry("mods/new.jar", "added", { counted: true, size: 10 }),
  entry("config/gen.json", "added", { size: 3 }),
  entry("config/once.toml", "onceChanged", { size: 5 }),
  entry("logs/latest.log", "addedExcluded", { size: 1 }),
];

test("countByClass counts every class, zero for the absent ones", () => {
  const counts = countByClass(sample);
  assert.deepEqual(Object.keys(counts).sort(), CLASS_ORDER.slice().sort());
  assert.equal(counts.modified, 1);
  assert.equal(counts.added, 2);
  assert.equal(counts.addedExcluded, 1);
  assert.deepEqual(countByClass([]).modified, 0);
});

test("countedTotal only counts the entries the backend marks counted", () => {
  assert.equal(countedTotal(sample), 4);
});

test("filterEntries keeps one class, or everything without a filter", () => {
  assert.deepEqual(
    filterEntries(sample, "added").map((e) => e.path),
    ["mods/new.jar", "config/gen.json"]
  );
  assert.equal(filterEntries(sample, null), sample);
});

test("splitByGroup moves the added files that will not publish out of the tree", () => {
  const { main, excluded } = splitByGroup(sample);
  assert.equal(main.length, sample.length - 1);
  assert.ok(main.every((e) => e.class !== "addedExcluded"));
  assert.deepEqual(excluded.map((e) => e.path), ["logs/latest.log"]);
});

test("buildChangeTree groups by directory and counts the entries below each", () => {
  const tree = buildChangeTree(splitByGroup(sample).main);
  assert.deepEqual(tree.map((n) => n.name), ["config", "mods"]);
  const [config, mods] = tree;
  assert.equal(config.kind, "dir");
  assert.equal(config.count, 4);
  assert.equal(mods.count, 2);
  assert.deepEqual(
    config.children.map((n) => n.name),
    ["a.toml", "b.toml", "gen.json", "once.toml"]
  );
  assert.equal(config.children[0].entry.class, "modified");
  assert.equal(config.children[0].count, 1);
});

test("buildChangeTree lists directories before files and nests deeper paths", () => {
  const tree = buildChangeTree([
    entry("z.txt", "added"),
    entry("a/b/c.txt", "modified"),
    entry("a/d.txt", "modified"),
    entry("b.txt", "added"),
  ]);
  assert.deepEqual(tree.map((n) => n.name), ["a", "b.txt", "z.txt"]);
  const [a] = tree;
  assert.equal(a.count, 2);
  assert.deepEqual(a.children.map((n) => n.name), ["b", "d.txt"]);
  assert.equal(a.children[0].path, "a/b");
  assert.equal(a.children[0].children[0].path, "a/b/c.txt");
});

test("buildChangeTree merges directories whose names differ only by case", () => {
  const tree = buildChangeTree([
    entry("Config/a.toml", "modified"),
    entry("config/b.toml", "modified"),
  ]);
  assert.equal(tree.length, 1);
  assert.equal(tree[0].path, "Config");
  assert.equal(tree[0].count, 2);
});

test("expandedDepth opens everything up to OPEN_LIMIT entries, else the first level", () => {
  assert.equal(OPEN_LIMIT, 5);
  assert.equal(expandedDepth(0), Infinity);
  assert.equal(expandedDepth(OPEN_LIMIT), Infinity);
  assert.equal(expandedDepth(OPEN_LIMIT + 1), 1);
});

test("revealPath points at the variant on disk and is null for gone files", () => {
  assert.equal(revealPath(entry("mods/x.jar", "toggled", { disabled: true })), "mods/x.jar.disabled");
  assert.equal(revealPath(entry("mods/y.jar", "added")), "mods/y.jar");
  assert.equal(revealPath(entry("config/b.toml", "deleted")), null);
  assert.equal(revealPath(entry("config/once.toml", "onceChanged")), null);
  assert.equal(revealPath(entry("config/once.toml", "onceChanged", { size: 5 })), "config/once.toml");
});
