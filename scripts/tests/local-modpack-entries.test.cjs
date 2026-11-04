const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function load(file, requireModule = () => ({})) {
  const module = { exports: {} };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(
      fs.readFileSync(path.join(__dirname, "../../", file), "utf8"),
      {
        compilerOptions: {
          module: ts.ModuleKind.CommonJS,
          jsx: ts.JsxEmit.React,
        },
      }
    ).outputText
  )(module.exports, module, requireModule);
  return module.exports;
}

const read = (file) =>
  fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
const helpers = load("src/utils/installing-instance.ts");
const ledger = load("src/utils/modpack-ledger.ts", (name) =>
  name === "@/utils/installing-instance"
    ? helpers
    : { ModLoaderType: { Unknown: "Unknown" } }
);

const instance = (id, platform, pending = false) => ({
  id,
  name: id,
  campusInstallPending: pending,
  origin: { platform, follow: platform !== "local" },
});

test("view changes of a local import needs a finished scan and no installation", () => {
  const local = instance("a", "local");
  assert.equal(ledger.ledgerLocalChangesDisabled(local, undefined), true);
  // No baseline: the scan yields no count.
  assert.equal(
    ledger.ledgerLocalChangesDisabled(local, { status: "notFollowing" }),
    true
  );
  // A count of 0 still means the comparison ran.
  assert.equal(
    ledger.ledgerLocalChangesDisabled(local, {
      status: "notFollowing",
      localChanges: 0,
    }),
    false
  );
  assert.equal(
    ledger.ledgerLocalChangesDisabled(instance("b", "local", true), {
      status: "notFollowing",
      localChanges: 3,
    }),
    true
  );
});

test("updatable local imports are the ledger's local rows that are not installing", () => {
  const list = [
    instance("local", "local"),
    instance("installing", "local", true),
    instance("campus", "campus"),
    { id: "plain", name: "plain" },
  ];
  assert.deepEqual(
    ledger.ledgerLocalUpdatable(list).map((item) => item.id),
    ["local"]
  );
});

test("the ledger's local rows open the changes dialog and are scanned", () => {
  const source = read("src/components/sync/instance-ledger.tsx");
  assert.match(source, /ledgerLocalChangesDisabled\(instance, check\)/);
  assert.match(source, /openSharedModal\("modpack-local-changes"/);
  assert.match(source, /\[followedKey, localKey\]/);
});

test("the import dialog switches to updating an existing instance through the shared staging", () => {
  const modal = read("src/components/modals/import-modpack-modal.tsx");
  const hook = read("src/hooks/local-modpack-update.tsx");
  // Mode switch with create as the default.
  assert.match(modal, /useState<ImportMode>\("create"\)/);
  assert.match(modal, /ImportModpackModal\.mode\.update/);
  // The candidates are the ledger's updatable local imports.
  assert.match(modal, /ledgerLocalUpdatable\(getInstanceList\(\) \?\? \[\]\)/);
  // Update mode stages the file already chosen, through the hook's path entry.
  assert.match(
    modal,
    /updateFromPath\(updateTarget\.id, updateTarget\.name, path\)/
  );
  assert.match(modal, /isDisabled=\{!updateTarget\}/);
  // The path entry owns staging; the file button only adds the picker.
  assert.match(
    hook,
    /stageLocalModpackUpdate\(\s*instanceId,\s*picked,\s*label/
  );
  assert.match(
    hook,
    /await updateFromPath\(instanceId, instanceName, picked\)/
  );
  assert.equal((hook.match(/stageLocalModpackUpdate\(/g) || []).length, 1);
  assert.match(hook, /return \{ updateFromFile, updateFromPath, pickingId \}/);
});

test("the ledger and the import dialog list the same local imports", () => {
  const ledgerSource = read("src/components/sync/instance-ledger.tsx");
  // Both exclude installing imports: the ledger by disabling the button, the
  // dialog by the shared filter.
  assert.match(
    ledgerSource,
    /isDisabled=\{installing \|\| pickingId !== null\}/
  );
  const list = [
    instance("a", "local"),
    instance("b", "local", true),
    instance("c", "modrinth"),
  ];
  const enabledInLedger = ledger
    .ledgerLocalInstances(list)
    .filter((item) => !helpers.isInstallingInstance(item))
    .map((item) => item.id);
  assert.deepEqual(
    ledger.ledgerLocalUpdatable(list).map((item) => item.id),
    enabledInLedger
  );
});
