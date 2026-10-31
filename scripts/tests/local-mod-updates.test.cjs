const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

// Transpile the TS module on the fly; it has no runtime imports.
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

const { createLocalModUpdateBuffer, mergeLocalModUpdates } = load(
  "src/utils/local-mod-updates.ts"
);

const mod = (filePath, translatedName, translatedDescription) => ({
  filePath,
  name: filePath,
  translatedName,
  translatedDescription,
});

test("updates are matched to mods by file path", () => {
  const list = [mod("/m/a.jar"), mod("/m/b.jar")];
  const merged = mergeLocalModUpdates(list, [
    {
      filePath: "/m/b.jar",
      translatedName: "乙",
      translatedDescription: "描述",
    },
    { filePath: "/m/missing.jar", translatedName: "无" },
  ]);
  assert.equal(merged[0], list[0]);
  assert.deepEqual(merged[1], {
    filePath: "/m/b.jar",
    name: "/m/b.jar",
    translatedName: "乙",
    translatedDescription: "描述",
  });
  // The input list is left untouched.
  assert.equal(list[1].translatedName, undefined);
});

test("an update without texts clears what a replaced jar left behind", () => {
  const merged = mergeLocalModUpdates(
    [mod("/m/a.jar", "旧", "旧描述")],
    [
      {
        filePath: "/m/a.jar",
        translatedName: null,
        translatedDescription: null,
      },
    ]
  );
  assert.equal(merged[0].translatedName, undefined);
  assert.equal(merged[0].translatedDescription, undefined);
});

test("the same list is returned when nothing changes", () => {
  const list = [mod("/m/a.jar", "甲", "描述")];
  assert.equal(mergeLocalModUpdates(list, []), list);
  assert.equal(
    mergeLocalModUpdates(list, [
      {
        filePath: "/m/a.jar",
        translatedName: "甲",
        translatedDescription: "描述",
      },
    ]),
    list
  );
  assert.equal(
    mergeLocalModUpdates(list, [
      { filePath: "/m/other.jar", translatedName: "x" },
    ]),
    list
  );
});

test("a map of updates works like a list", () => {
  const updates = new Map([
    ["/m/a.jar", { filePath: "/m/a.jar", translatedName: "甲" }],
  ]);
  assert.equal(
    mergeLocalModUpdates([mod("/m/a.jar")], updates)[0].translatedName,
    "甲"
  );
});

test("the buffer keeps updates of the instance being loaded only", () => {
  const buffer = createLocalModUpdateBuffer();
  const update = (filePath, translatedName) => ({ filePath, translatedName });

  // Nothing is collected before a load has started.
  assert.equal(
    buffer.add({ instanceId: "a", mods: [update("/a/1.jar", "一")] }),
    false
  );

  buffer.reset("a");
  assert.equal(
    buffer.add({ instanceId: "a", mods: [update("/a/1.jar", "一")] }),
    true
  );
  assert.equal(
    buffer.add({ instanceId: "b", mods: [update("/b/1.jar", "壹")] }),
    false
  );
  buffer.add({
    instanceId: "a",
    mods: [update("/a/1.jar", "一二"), update("/a/2.jar", "二")],
  });
  assert.deepEqual(
    Array.from(buffer.updates().values()).map((item) => item.translatedName),
    ["一二", "二"]
  );

  // Switching instance drops the previous instance's updates.
  buffer.reset("b");
  assert.equal(buffer.updates().size, 0);
  assert.equal(
    buffer.add({ instanceId: "a", mods: [update("/a/1.jar", "一")] }),
    false
  );
});

test("updates that arrive before the list are applied to it afterwards", () => {
  const buffer = createLocalModUpdateBuffer();
  buffer.reset("a");
  buffer.add({
    instanceId: "a",
    mods: [
      {
        filePath: "/a/1.jar",
        translatedName: "一",
        translatedDescription: "d",
      },
    ],
  });
  const loaded = [mod("/a/1.jar"), mod("/a/2.jar")];
  const merged = mergeLocalModUpdates(loaded, buffer.updates());
  assert.equal(merged[0].translatedName, "一");
  assert.equal(merged[1], loaded[1]);
});
