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

const { resolvePlanChoices } = load("src/utils/modpack-update-choices.ts");

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
