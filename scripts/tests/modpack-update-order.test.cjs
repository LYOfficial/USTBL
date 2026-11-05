const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

const root = path.join(__dirname, "../../");

// Transpile the TS module on the fly; it only imports types.
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
  )(exports, module, (name) =>
    name === "@/utils/diff-rank"
      ? load("src/utils/diff-rank.ts")
      : name === "@/utils/path-tree"
        ? load("src/utils/path-tree.ts")
        : {}
  );
  return module.exports;
}

const order = load("src/utils/modpack-update-order.ts");

const conflict = (p, kind, hunks = 0) => ({
  path: p,
  kind,
  hunks: Array.from({ length: hunks }, () => ({})),
});
const change = (p, extra = {}) => ({
  path: p,
  kind: "update",
  localSize: 10,
  remote: { sha1: "a", size: 20 },
  ...extra,
});

test("the text extensions match the backend's list", () => {
  const source = fs.readFileSync(
    path.join(root, "src-tauri/src/instance/helpers/modpack/text.rs"),
    "utf8"
  );
  const body = /TEXT_EXTENSIONS: &\[&str\] = &\[([^\]]*)\]/.exec(source)[1];
  const backend = [...body.matchAll(/"([^"]+)"/g)].map((match) => match[1]);
  assert.deepEqual([...order.TEXT_EXTENSIONS].sort(), backend.sort());
  const size = /MAX_TEXT_SIZE: usize = ([0-9 *]+);/.exec(source)[1];
  assert.equal(order.MAX_TEXT_SIZE, eval(size));
});

test(".lang and .properties files are text, a name without extension is not", () => {
  assert.equal(order.hasTextExtension("assets/x/lang/en_US.lang"), true);
  assert.equal(order.hasTextExtension("config/a.PROPERTIES"), true);
  assert.equal(order.hasTextExtension("mods/a.jar"), false);
  assert.equal(order.hasTextExtension("config/.json"), false);
  assert.equal(order.hasTextExtension("config/toml"), false);
});

test("only unrestored text updates within the size limit have a diff", () => {
  assert.equal(order.canShowChangeDiff(change("config/a.cfg")), true);
  assert.equal(order.canShowChangeDiff(change("mods/a.jar")), false);
  assert.equal(
    order.canShowChangeDiff(change("config/a.cfg", { restored: true })),
    false
  );
  assert.equal(
    order.canShowChangeDiff(change("config/a.cfg", { kind: "add" })),
    false
  );
  assert.equal(
    order.canShowChangeDiff(
      change("config/a.cfg", { localSize: order.MAX_TEXT_SIZE + 1 })
    ),
    false
  );
  assert.equal(
    order.canShowChangeDiff(
      change("config/a.cfg", {
        remote: { sha1: "a", size: order.MAX_TEXT_SIZE + 1 },
      })
    ),
    false
  );
});

test("conflicts sort by blocks, whole-file replacement, then the rest", () => {
  const list = [
    conflict("a.jar", "modifiedLocallyRemovedRemotely"),
    conflict("b.png", "modifiedLocallyChangedRemotely"),
    conflict("c.toml", "modifiedLocallyChangedRemotely", 2),
    conflict("d.cfg", "deletedLocallyChangedRemotely"),
    conflict("e.lang", "unmanagedFileInTheWay"),
    conflict("f.json", "modifiedLocallyChangedRemotely", 1),
    conflict("g.jar", "remanage"),
  ];
  const before = list.map((item) => item.path);
  assert.deepEqual(
    order.sortConflicts(list).map((item) => item.path),
    ["c.toml", "f.json", "b.png", "e.lang", "g.jar", "a.jar", "d.cfg"]
  );
  // The plan's own list is untouched: choices and the plan hash follow it.
  assert.deepEqual(
    list.map((item) => item.path),
    before
  );
});

test("changes with a diff come first, each part by path", () => {
  const list = [
    change("mods/b.jar"),
    change("config/z.cfg"),
    change("mods/a.jar"),
    change("config/a.lang"),
  ];
  assert.deepEqual(
    order.sortChangesByDiff(list).map((item) => item.path),
    ["config/a.lang", "config/z.cfg", "mods/a.jar", "mods/b.jar"]
  );
});
