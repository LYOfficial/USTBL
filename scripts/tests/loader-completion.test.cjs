const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

const exportsFor = {};
new Function(
  "exports",
  "require",
  ts.transpileModule(
    fs.readFileSync(
      path.join(__dirname, "../../src/utils/loader-completion.ts"),
      "utf8"
    ),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2020,
      },
    }
  ).outputText
)(exportsFor, () => ({ GTaskEventStatusEnums: { Completed: "Completed" } }));
const { takeLoaderCompletions } = exportsFor;
const group = (name, id, status = "Completed", time = 1) => ({
  taskGroup: `${name}?${id}@${time}`,
  status,
  taskDescs: [],
});

test("replayed updates and settled callbacks claim a batch only once", () => {
  const claims = new Map();
  const groups = [group("neoforge-libraries", "default:航空学")];
  assert.deepEqual(takeLoaderCompletions(groups, claims), ["default:航空学"]);
  assert.deepEqual(takeLoaderCompletions(groups, claims), []);
  assert.deepEqual(takeLoaderCompletions(structuredClone(groups), claims), []);
});

test("different instances are not blocked by a global loading toast", () => {
  assert.deepEqual(
    takeLoaderCompletions(
      [group("forge-libraries", "a"), group("neoforge-libraries", "b")],
      new Map()
    ),
    ["a", "b"]
  );
});

test("Forge plus OptiFine waits for both runtime groups", () => {
  const claims = new Map();
  const groups = [
    group("forge-libraries", "a"),
    group("optifine-libraries", "a", "Started"),
  ];
  assert.deepEqual(takeLoaderCompletions(groups, claims), []);
  groups[1].status = "Completed";
  assert.deepEqual(takeLoaderCompletions(groups, claims), ["a"]);
  assert.deepEqual(takeLoaderCompletions(groups, claims), []);
});

test("failed and stopped groups do not trigger processors; a new batch can retry", () => {
  const claims = new Map();
  for (const status of ["Started", "Stopped", "Failed", "Cancelled"]) {
    assert.deepEqual(
      takeLoaderCompletions([group("forge-libraries", "a", status)], claims),
      []
    );
  }
  assert.deepEqual(
    takeLoaderCompletions([group("forge-libraries", "a")], claims),
    ["a"]
  );
  const groups = [
    group("forge-libraries", "a"),
    group("forge-libraries", "a", "Started", 2),
  ];
  assert.deepEqual(takeLoaderCompletions(groups, claims), []);
  groups[1].status = "Completed";
  assert.deepEqual(takeLoaderCompletions(groups, claims), ["a"]);
});

test("older failed history does not block a successful retry or an empty OptiFine group", () => {
  assert.deepEqual(
    takeLoaderCompletions(
      [
        group("optifine-libraries", "a", "Completed", 2),
        group("optifine-libraries", "a", "Failed", 1),
        group("game-client", "b"),
      ],
      new Map()
    ),
    ["a"]
  );
});

test("installer-only completion never triggers loader processors", () => {
  assert.deepEqual(
    takeLoaderCompletions([group("loader-installer", "a")], new Map()),
    []
  );
});

test("clearing part of the completed history does not repeat completion", () => {
  const claims = new Map();
  const groups = [
    group("forge-libraries", "a"),
    group("optifine-libraries", "a"),
  ];
  assert.deepEqual(takeLoaderCompletions(groups, claims), ["a"]);
  assert.deepEqual(takeLoaderCompletions(groups.slice(1), claims), []);
});

test("one instance cannot invoke again while its callback is in flight", () => {
  const claims = new Map();
  const groups = [group("forge-libraries", "a")];
  assert.deepEqual(takeLoaderCompletions(groups, claims), ["a"]);
  groups.push(group("optifine-libraries", "a"));
  assert.deepEqual(takeLoaderCompletions(groups, claims, new Set(["a"])), []);
  assert.deepEqual(takeLoaderCompletions(groups, claims), ["a"]);
  assert.deepEqual(takeLoaderCompletions(groups, claims), []);
});
