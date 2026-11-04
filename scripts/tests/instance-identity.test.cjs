const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function read(file) {
  return fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
}

function load(file, modules = {}) {
  const exports = {};
  const module = { exports };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(read(file), {
      compilerOptions: { module: ts.ModuleKind.CommonJS },
    }).outputText
  )(exports, module, (name) => modules[name] ?? {});
  return module.exports;
}

const { findInstanceById } = load("src/utils/instance.ts", {
  "@/enums/instance": { ModLoaderType: { Unknown: "Unknown" } },
  "@/utils/string": { isDirNameInvalid: () => 0 },
});

const instances = [
  { id: "7d6f0a2e-0000-4000-8000-000000000001", gameDir: "games", folder: "a" },
  { id: "7d6f0a2e-0000-4000-8000-000000000002", gameDir: "other", folder: "a" },
  {
    id: "7d6f0a2e-0000-4000-8000-000000000003",
    gameDir: "D:\\mc",
    folder: "b",
  },
];

test("an instance is found by its current id", () => {
  assert.equal(findInstanceById(instances, instances[1].id), instances[1]);
});

test("an id from an older version is found by game directory and folder", () => {
  assert.equal(findInstanceById(instances, "other:a"), instances[1]);
  assert.equal(findInstanceById(instances, "games:a"), instances[0]);
  assert.equal(findInstanceById(instances, "D:\\mc:b"), instances[2]);
  assert.equal(findInstanceById(instances, "games:missing"), undefined);
  assert.equal(findInstanceById(instances, "a"), undefined);
  assert.equal(findInstanceById(undefined, "games:a"), undefined);
});

test("the list page filters by game directory instead of reading the id", () => {
  const source = read("src/pages/instances/list/[[...dir]].tsx");
  assert.match(source, /inst\.gameDir === gameDir/);
  assert.doesNotMatch(source, /id\.startsWith/);
});

test("renamed instances can get a desktop shortcut", () => {
  const layout = read("src/layouts/instance-details-layout.tsx");
  assert.doesNotMatch(layout, /CreateRenamedInstShortcutAlertDialog/);
  assert.doesNotMatch(layout, /indexOf\(":"\)/);
  for (const locale of ["zh-Hans", "zh-Hant", "en", "ja", "fr"]) {
    const json = JSON.parse(read(`src/locales/${locale}.json`));
    assert.equal(json.CreateRenamedInstShortcutAlertDialog, undefined);
  }
});

test("renaming writes the display name only", () => {
  const settings = read("src/pages/instances/details/[id]/settings/index.tsx");
  assert.doesNotMatch(settings, /updateSummaryInContext\("versionPath"/);
  assert.match(settings, /useRenameInstance/);
  const layout = read("src/layouts/instance-details-layout.tsx");
  assert.match(layout, /<InstanceNameEditor/);
});

test("changing a game directory name no longer rewrites instance detail routes", () => {
  const modal = read("src/components/modals/edit-game-directory-modal.tsx");
  assert.doesNotMatch(modal, /instances\/details\/\$\{currentName\}:/);
  assert.doesNotMatch(modal, /replaceHistory\(\s*`\/instances\/details/);
  // moving the directory drops the history of the instances it held
  assert.match(modal, /removeHistory\(`\/instances\/details\/\$\{instance\.id\}`\)/);
  assert.match(modal, /\/instances\/list\/\$\{currentName\}/);
});

test("modals take the new instance id from createInstance", () => {
  for (const file of [
    "src/components/modals/import-modpack-modal.tsx",
    "src/components/modals/install-campus-modpack-modal.tsx",
    "src/components/modals/install-platform-modpack-modal.tsx",
  ]) {
    const source = read(file);
    assert.match(source, /createResp\.data/, file);
    assert.doesNotMatch(source, /\$\{gameDirectory\.name\}:\$\{name\}/, file);
  }
});

test("every instance creation entry checks the folder first", () => {
  for (const file of [
    "src/components/modals/create-instance-modal.tsx",
    "src/components/modals/import-modpack-modal.tsx",
    "src/components/modals/install-campus-modpack-modal.tsx",
    "src/components/modals/install-platform-modpack-modal.tsx",
  ]) {
    const source = read(file);
    const check = source.indexOf("resolveFolder(");
    const create = source.indexOf("InstanceService.createInstance(");
    assert.ok(check !== -1 && check < create, file);
    assert.match(source, /\{folderConflictDialog\}/, file);
    assert.doesNotMatch(
      source.slice(create, create + 80),
      /\bname,/,
      `${file} must create with the resolved folder`
    );
  }
});

test("a renamed-version suggestion swaps only the old label", () => {
  const { suggestRenamedName, takeRenameSuggestion, pendingUpdateLabels } = load(
    "src/utils/rename-suggestion.ts"
  );
  assert.equal(suggestRenamedName("GTNH 2.8.3", "2.8.3", "2.8.4"), "GTNH 2.8.4");
  assert.equal(suggestRenamedName("GTNH", "2.8.3", "2.8.4"), null);
  assert.equal(suggestRenamedName("GTNH 2.8.3", "", "2.8.4"), null);
  assert.equal(suggestRenamedName("GTNH 2.8.3", undefined, "2.8.4"), null);
  assert.equal(suggestRenamedName("GTNH 2.8.3", "2.8.3", "2.8.3"), null);
  pendingUpdateLabels.set("a", { from: "1.0", to: "1.1" });
  assert.deepEqual(takeRenameSuggestion("a", "Pack 1.0"), {
    version: "1.1",
    suggested: "Pack 1.1",
  });
  assert.equal(takeRenameSuggestion("a", "Pack 1.0"), null);
  assert.equal(takeRenameSuggestion("b", "Pack 1.0"), null);
});

test("suggestion dialogs take turns", () => {
  const { enqueueSuggestionModal } = load("src/utils/modal-queue.ts");
  const order = [];
  let releaseFirst;
  enqueueSuggestionModal((release) => {
    order.push("first");
    releaseFirst = release;
  });
  enqueueSuggestionModal((release) => {
    order.push("second");
    release();
  });
  assert.deepEqual(order, ["first"]);
  releaseFirst();
  releaseFirst();
  assert.deepEqual(order, ["first", "second"]);
});

test("the update flow notes labels, suggests after success and queues behind the memory prompt", () => {
  assert.match(read("src/components/modals/update-modpack-modal.tsx"), /pendingUpdateLabels\.set/);
  assert.match(read("src/utils/modpack-update-task.ts"), /suggestRename\?\.\(/);
  assert.match(read("src/contexts/task.tsx"), /enqueueSuggestionModal/);
  assert.match(read("src/hooks/memory-suggestion.ts"), /enqueueSuggestionModal/);
});
