const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function load(file, requireModule = () => ({})) {
  const exports = {};
  const module = { exports };
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
  )(exports, module, requireModule);
  return module.exports;
}

const {
  formatMemoryMb,
  isMemorySuggestionCapped,
  memorySuggestionKey,
  nextMemorySuggestion,
} = load("src/utils/memory-suggestion.ts");

const suggestion = (instanceId, suggestedMb, overrides = {}) => ({
  instanceId,
  packName: instanceId,
  suggestedMb,
  cappedMb: suggestedMb,
  totalMb: 16384,
  currentMb: 2048,
  currentSource: "global",
  ...overrides,
});

test("a suggestion is keyed by instance and recommended value", () => {
  assert.notEqual(
    memorySuggestionKey(suggestion("a", 8196)),
    memorySuggestionKey(suggestion("a", 10240))
  );
  assert.notEqual(
    memorySuggestionKey(suggestion("a", 8196)),
    memorySuggestionKey(suggestion("b", 8196))
  );
});

test("one suggestion at a time, skipping the dismissed ones", () => {
  const all = [suggestion("a", 8196), suggestion("b", 4096)];
  assert.equal(nextMemorySuggestion([], new Set()), undefined);
  assert.equal(nextMemorySuggestion(all, new Set()).instanceId, "a");
  const dismissed = new Set([memorySuggestionKey(all[0])]);
  assert.equal(nextMemorySuggestion(all, dismissed).instanceId, "b");
  dismissed.add(memorySuggestionKey(all[1]));
  assert.equal(nextMemorySuggestion(all, dismissed), undefined);
});

test("a dismissed instance is asked again when its recommendation changes", () => {
  const dismissed = new Set([memorySuggestionKey(suggestion("a", 8196))]);
  assert.equal(
    nextMemorySuggestion([suggestion("a", 10240)], dismissed).suggestedMb,
    10240
  );
});

test("the cap notice shows only when the value was lowered", () => {
  assert.equal(isMemorySuggestionCapped(suggestion("a", 8196)), false);
  assert.equal(
    isMemorySuggestionCapped(suggestion("a", 8196, { cappedMb: 7372 })),
    true
  );
});

test("memory is shown in MB like the settings page", () => {
  assert.equal(formatMemoryMb(8196), "8196 MB");
});
