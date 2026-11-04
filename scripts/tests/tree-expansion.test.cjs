const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function load(file) {
  const exports = {};
  const module = { exports };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(
      fs.readFileSync(path.join(__dirname, "../../", file), "utf8"),
      {
        compilerOptions: { module: ts.ModuleKind.CommonJS },
      }
    ).outputText
  )(exports, module, () => ({}));
  return module.exports;
}

const { applyBulkExpansion, applyExpansionRequest, syncExpandedState } = load(
  "src/utils/tree-expansion.ts"
);

const tree = (ids) =>
  ids.map((id) => ({
    id,
    data: null,
    children: [{ id: `${id}/file`, data: null }],
  }));

test("a user-collapsed default branch stays collapsed when nodes change", () => {
  const history = new Map();
  const nodes = tree(["a"]);
  syncExpandedState(history, new Set(), nodes, new Set(["a"]));
  history.set("a", false);
  const result = syncExpandedState(
    history,
    new Set(),
    tree(["a", "b"]),
    new Set(["a", "b"])
  );
  assert.deepEqual([...result], ["b"]);
});

test("a collapsed branch stays collapsed during filtered state changes", () => {
  const history = new Map([["a", false]]);
  const result = syncExpandedState(history, new Set(), tree(["a"]), new Set());
  assert.deepEqual([...result], []);
});

test("a filtered-out branch keeps its expansion when it returns", () => {
  const history = new Map([["a", true]]);
  const result = syncExpandedState(history, new Set(), tree(["a"]), new Set());
  assert.deepEqual([...result], ["a"]);
});

test("a branch that never appeared uses the default depth", () => {
  const result = syncExpandedState(
    new Map(),
    new Set(),
    tree(["a"]),
    new Set(["a"])
  );
  assert.deepEqual([...result], ["a"]);
});

test("the same expansion request is applied once by request identity", () => {
  const history = new Map();
  const request = new Set(["a"]);
  applyExpansionRequest(history, request);
  history.set("a", false);
  applyExpansionRequest(history, request);
  assert.equal(history.get("a"), true);
});

test("expand all and collapse all are remembered for every branch", () => {
  const history = new Map([["a", false], ["b", true]]);
  const nodes = tree(["a", "b"]);
  const branches = new Set(["a", "b"]);
  applyBulkExpansion(history, branches, true);
  assert.deepEqual(
    [...syncExpandedState(history, new Set(), nodes, new Set())].sort(),
    ["a", "b"]
  );
  applyBulkExpansion(history, branches, false);
  assert.deepEqual(
    [...syncExpandedState(history, new Set(), nodes, new Set(["a"]))],
    []
  );
});
