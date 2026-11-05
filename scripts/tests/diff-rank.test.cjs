const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function load(file) {
  const exports = {};
  const mod = { exports };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(
      fs.readFileSync(path.join(__dirname, "../../", file), "utf8"),
      { compilerOptions: { module: ts.ModuleKind.CommonJS } }
    ).outputText
  )(exports, mod, (name) =>
    name === "@/utils/path-tree"
      ? load("src/utils/path-tree.ts")
      : name === "@/utils/diff-rank"
        ? load("src/utils/diff-rank.ts")
        : {}
  );
  return mod.exports;
}

const { sortByRank } = load("src/utils/diff-rank.ts");
const detail = load("src/utils/modpack-change-detail.ts");
const order = load("src/utils/modpack-update-order.ts");

test("sortByRank orders by rank, then compare, then original order", () => {
  const items = [
    { n: "b", r: 1 },
    { n: "a", r: 1 },
    { n: "z", r: 0 },
    { n: "a", r: 1, tag: "second" },
  ];
  const byRank = sortByRank(items, (i) => i.r);
  assert.deepEqual(
    byRank.map((i) => i.n),
    ["z", "b", "a", "a"]
  );
  const named = sortByRank(
    items,
    (i) => i.r,
    (a, b) => a.n.localeCompare(b.n)
  );
  assert.deepEqual(
    named.map((i) => i.n),
    ["z", "a", "a", "b"]
  );
  assert.equal(named[2].tag, "second");
  assert.equal(items[0].n, "b");
});

test("entryRank and ignoredRank", () => {
  const e = (cls, hunks) => ({ path: "a", class: cls, hunks });
  assert.equal(detail.entryRank(e("modified", [{}])), 0);
  assert.equal(detail.entryRank(e("modified", [])), 1);
  assert.equal(detail.entryRank(e("modified", null)), 1);
  assert.equal(detail.entryRank(e("added", [{}])), 2);
  assert.equal(detail.ignoredRank({ added: true }), 2);
  assert.equal(detail.ignoredRank({ whole: true }), 1);
  assert.equal(detail.ignoredRank({}), 0);
  const sorted = detail.sortEntriesByDiff([
    e("added"),
    e("modified", null),
    e("modified", [{}]),
  ]);
  assert.deepEqual(
    sorted.map((x) => detail.entryRank(x)),
    [0, 1, 2]
  );
});

test("changeRank puts diffable updates first, other updates next", () => {
  const update = {
    path: "a.cfg",
    kind: "update",
    localSize: 1,
    remote: { sha1: "a", size: 2 },
  };
  const jar = {
    path: "a.jar",
    kind: "update",
    localSize: 1,
    remote: { sha1: "a", size: 2 },
  };
  const add = { path: "b.cfg", kind: "add", remote: { sha1: "a", size: 2 } };
  assert.equal(order.changeRank(update), 0);
  assert.equal(order.changeRank(jar), 1);
  assert.equal(order.changeRank(add), 2);
  assert.deepEqual(
    order.sortChangesByDiff([add, jar, update]).map((c) => c.path),
    ["a.cfg", "a.jar", "b.cfg"]
  );
});
