const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

// Transpile the TS module on the fly; it only imports a type, so no runtime
// dependencies need stubbing.
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
  resolvePlanChoices,
  withDefaultChoices,
  choiceOf,
  DEFAULT_CONFLICT_CHOICE,
} = load("src/utils/modpack-update-choices.ts");

const hunkChoice = { "config/fadeless.json#0": "takeRemote" };

test("a re-check after a failed apply keeps the choices for the same plan", () => {
  const result = resolvePlanChoices({
    instanceId: "i1",
    newPlanHash: "hash-a",
    failedPlanHash: "hash-a",
    recheckAfterFailure: true,
    remembered: new Map(),
    currentChoices: hunkChoice,
  });
  assert.deepEqual(result.choices, hunkChoice);
  assert.equal(result.toastPlanChanged, false);
});

test("a re-check after a failed apply clears and announces a changed plan", () => {
  const result = resolvePlanChoices({
    instanceId: "i1",
    newPlanHash: "hash-b",
    failedPlanHash: "hash-a",
    recheckAfterFailure: true,
    remembered: new Map(),
    currentChoices: hunkChoice,
  });
  assert.deepEqual(result.choices, {});
  assert.equal(result.toastPlanChanged, true);
});

test("a plain open restores the remembered choices of the same plan only", () => {
  const remembered = new Map([
    ["i1", { planHash: "hash-a", choices: hunkChoice }],
  ]);
  const restored = resolvePlanChoices({
    instanceId: "i1",
    newPlanHash: "hash-a",
    failedPlanHash: null,
    recheckAfterFailure: false,
    remembered,
    currentChoices: {},
  });
  assert.deepEqual(restored.choices, hunkChoice);

  const otherPlan = resolvePlanChoices({
    instanceId: "i1",
    newPlanHash: "hash-b",
    failedPlanHash: null,
    recheckAfterFailure: false,
    remembered,
    currentChoices: {},
  });
  assert.deepEqual(otherPlan.choices, {});
  assert.equal(otherPlan.toastPlanChanged, false);

  const otherInstance = resolvePlanChoices({
    instanceId: "i2",
    newPlanHash: "hash-a",
    failedPlanHash: null,
    recheckAfterFailure: false,
    remembered,
    currentChoices: {},
  });
  assert.deepEqual(otherInstance.choices, {});
});

test("a failed plan whose hash was never recorded still clears on a new plan", () => {
  const result = resolvePlanChoices({
    instanceId: "i1",
    newPlanHash: "hash-a",
    failedPlanHash: null,
    recheckAfterFailure: true,
    remembered: new Map(),
    currentChoices: hunkChoice,
  });
  assert.deepEqual(result.choices, {});
  assert.equal(result.toastPlanChanged, true);
});

test("the caller passes the failed hash explicitly: the same plan keeps the choices", () => {
  // The dialog cannot rely on plan state surviving the re-check (runCheck
  // clears it before the new prepare answers), so the failed plan's hash
  // arrives as an argument.
  const kept = resolvePlanChoices({
    instanceId: "i1",
    newPlanHash: "hash-a",
    failedPlanHash: "hash-a",
    recheckAfterFailure: true,
    remembered: new Map(),
    currentChoices: hunkChoice,
  });
  assert.deepEqual(kept.choices, hunkChoice);
  assert.equal(kept.toastPlanChanged, false);
});

const conflicts = [
  { path: "config/a.toml", hunks: [{}, {}] },
  { path: "mods/b.jar", hunks: [] },
  { path: "config/c.cfg", hunks: [] },
];

test("an unanswered conflict defaults to the new version, same as the backend", () => {
  assert.equal(DEFAULT_CONFLICT_CHOICE, "takeRemote");
  assert.equal(choiceOf({}, "mods/b.jar"), "takeRemote");
  assert.equal(
    choiceOf({ "mods/b.jar": "keepLocal" }, "mods/b.jar"),
    "keepLocal"
  );
  const backend = fs.readFileSync(
    path.join(__dirname, "../../src-tauri/src/instance/modpack_update/diff.rs"),
    "utf8"
  );
  assert.match(
    backend,
    /pub enum ConflictChoice \{\s*KeepLocal,\s*#\[default\]\s*TakeRemote,/
  );
});

test("the submitted answers cover every conflict and block", () => {
  const sent = withDefaultChoices(conflicts, {
    "config/a.toml#1": "keepLocal",
    "mods/b.jar": "keepLocal",
    "gone.cfg": "keepLocal",
  });
  assert.deepEqual(sent, {
    "config/a.toml#0": "takeRemote",
    "config/a.toml#1": "keepLocal",
    "mods/b.jar": "keepLocal",
    "config/c.cfg": "takeRemote",
  });
});

test("remembered answers of the same plan are restored, defaults fill the rest", () => {
  const remembered = new Map([
    ["i1", { planHash: "h", choices: { "mods/b.jar": "keepLocal" } }],
  ]);
  const { choices } = resolvePlanChoices({
    instanceId: "i1",
    newPlanHash: "h",
    failedPlanHash: null,
    recheckAfterFailure: false,
    remembered,
    currentChoices: {},
  });
  assert.equal(
    withDefaultChoices(conflicts, choices)["mods/b.jar"],
    "keepLocal"
  );
  assert.equal(
    withDefaultChoices(conflicts, choices)["config/c.cfg"],
    "takeRemote"
  );
});

test("the dialog submits the filled-in answers", () => {
  const modal = fs.readFileSync(
    path.join(
      __dirname,
      "../../src/components/modals/update-modpack-modal.tsx"
    ),
    "utf8"
  );
  assert.match(modal, /withDefaultChoices\(plan\.conflicts, choices\)/);
});
