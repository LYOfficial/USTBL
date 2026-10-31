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
  expandedDepth,
  isRuleDeleted,
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
  // Covered by the previous release's rules.
  entry("mods/old.jar", "added", { counted: true, rule: "delete", size: 9 }),
  entry("saves/w/level.dat", "addedExcluded", { rule: "delete", size: 2 }),
  entry("config/skip.toml", "added", { rule: "ignore", size: 4 }),
  entry("mods/kept.jar", "added", { counted: true, rule: "ignore", size: 8 }),
];

test("the body lists only the counted entries", () => {
  const { main } = splitByGroup(sample);
  assert.ok(main.every((e) => e.counted));
  assert.deepEqual(main.map((e) => e.path).sort(), [
    "config/a.toml",
    "config/b.toml",
    "mods/kept.jar",
    "mods/new.jar",
    "mods/x.jar",
  ]);
});

test("added files under a delete rule go to the will-delete group, counted or not", () => {
  const { main, willDelete } = splitByGroup(sample);
  assert.deepEqual(willDelete.map((e) => e.path), [
    "mods/old.jar",
    "saves/w/level.dat",
  ]);
  assert.ok(willDelete.every(isRuleDeleted));
  assert.ok(main.every((e) => !isRuleDeleted(e)));
});

test("first-install changes and uncounted additions are not shown anywhere", () => {
  const { main, willDelete } = splitByGroup(sample);
  const shown = [...main, ...willDelete].map((e) => e.path);
  for (const hidden of [
    "config/once.toml",
    "config/gen.json",
    "logs/latest.log",
    "config/skip.toml",
  ]) {
    assert.ok(!shown.includes(hidden), hidden);
  }
});

test("a delete rule on a modified entry does not move it", () => {
  const { main, willDelete } = splitByGroup([
    entry("config/a.toml", "modified", { rule: "delete" }),
  ]);
  assert.equal(main.length, 1);
  assert.equal(willDelete.length, 0);
});

test("the counts cover the body only", () => {
  const counts = countByClass(splitByGroup(sample).main);
  assert.deepEqual(
    CLASS_ORDER.map((name) => counts[name]),
    [1, 1, 1, 2]
  );
  assert.equal(counts.onceChanged, 0);
  assert.equal(counts.addedExcluded, 0);
  assert.equal(countByClass([]).modified, 0);
});

test("filterEntries keeps one class of the body, or everything without a filter", () => {
  const { main } = splitByGroup(sample);
  assert.deepEqual(
    filterEntries(main, "added").map((e) => e.path),
    ["mods/new.jar", "mods/kept.jar"]
  );
  assert.equal(filterEntries(main, null), main);
});

test("buildChangeTree groups by directory and counts the entries below each", () => {
  const tree = buildChangeTree(splitByGroup(sample).main);
  assert.deepEqual(tree.map((n) => n.name), ["config", "mods"]);
  const [config, mods] = tree;
  assert.equal(config.kind, "dir");
  assert.equal(config.count, 2);
  assert.equal(mods.count, 3);
  assert.deepEqual(
    config.children.map((n) => n.name),
    ["a.toml", "b.toml"]
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
