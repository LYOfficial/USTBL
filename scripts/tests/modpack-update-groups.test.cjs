const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

// Transpile the TS module on the fly; it only imports types.
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

const { groupPlanChanges } = load("src/utils/modpack-update-groups.ts");

const change = (p, kind, extra = {}) => ({ path: p, kind, ...extra });

const plan = {
  changes: [
    change("mods/new.jar", "add"),
    change("mods/back.jar", "add", { restored: true }),
    change("config/a.toml", "update", { restored: true }),
    change("config/b.toml", "update"),
    change("mods/gone.jar", "delete", { restored: true }),
    change("mods/old.jar", "delete"),
    change("mods/x.jar", "disable"),
    change("mods/y.jar", "enable"),
    change("mods/z.jar", "retire"),
  ],
  autoMerged: [
    change("config/m.toml", "update"),
    change("config/n.toml", "update", { restored: true }),
  ],
};

test("restored changes, from changes and autoMerged, go to the restored group", () => {
  const groups = groupPlanChanges(plan);
  assert.deepEqual(groups.restored.map((c) => c.path).sort(), [
    "config/a.toml",
    "config/n.toml",
    "mods/back.jar",
    "mods/gone.jar",
  ]);
});

test("a restored change shows in no other group", () => {
  const groups = groupPlanChanges(plan);
  const others = [
    ...groups.add,
    ...groups.update,
    ...groups.delete,
    ...groups.disable,
    ...groups.enable,
    ...groups.retire,
    ...groups.autoMerged,
  ];
  assert.ok(others.every((c) => !c.restored));
  assert.deepEqual(groups.add.map((c) => c.path), ["mods/new.jar"]);
  assert.deepEqual(groups.update.map((c) => c.path), ["config/b.toml"]);
  assert.deepEqual(groups.delete.map((c) => c.path), ["mods/old.jar"]);
  assert.deepEqual(groups.autoMerged.map((c) => c.path), ["config/m.toml"]);
});

test("every change lands in exactly one group", () => {
  const groups = groupPlanChanges(plan);
  const total = Object.values(groups).reduce((sum, list) => sum + list.length, 0);
  assert.equal(total, plan.changes.length + plan.autoMerged.length);
});

test("without restored changes the groups follow the kinds", () => {
  const groups = groupPlanChanges({
    changes: [change("a", "add"), change("b", "update")],
    autoMerged: [],
  });
  assert.equal(groups.restored.length, 0);
  assert.equal(groups.add.length, 1);
  assert.equal(groups.update.length, 1);
});
