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

const {
  createRequestGate,
  mergeTranslations,
  mergeTranslationsForSource,
  pendingTranslationIds,
  pendingTranslationIdsBySource,
} = load("src/utils/resource-translation.ts");

const item = (id, translatedDescription) => ({
  id,
  name: id,
  translatedDescription,
});

test("pendingTranslationIds skips translated items and duplicates", () => {
  const list = [item("a"), item("b", "已译"), item("a"), item("c", "")];
  assert.deepEqual(pendingTranslationIds(list), ["a", "c"]);
  assert.deepEqual(pendingTranslationIds([]), []);
});

test("mergeTranslations fills only the matching ids and keeps other fields", () => {
  const list = [item("a"), item("b", "旧"), item("c")];
  const merged = mergeTranslations(list, { a: "甲", b: "乙", zzz: "丙" });
  assert.deepEqual(
    merged.map((entry) => entry.translatedDescription),
    ["甲", "乙", undefined]
  );
  assert.equal(merged[0].name, "a");
  // The input is not mutated.
  assert.equal(list[0].translatedDescription, undefined);
  assert.equal(merged[2], list[2]);
});

test("mergeTranslations returns the same array when nothing changes", () => {
  const list = [item("a", "甲"), item("b")];
  assert.equal(mergeTranslations(list, {}), list);
  assert.equal(mergeTranslations(list, { a: "甲" }), list);
  assert.equal(mergeTranslations(list, { b: "" }), list);
});

test("a reply for an old search is not current once a new search starts", () => {
  const gate = createRequestGate();
  const first = gate.next();
  assert.equal(gate.isCurrent(first), true);

  // Loading the next page of the same search keeps the token valid.
  assert.equal(gate.current(), first);
  assert.equal(gate.isCurrent(first), true);

  const second = gate.next();
  assert.equal(gate.isCurrent(first), false);
  assert.equal(gate.isCurrent(second), true);

  // The stale reply is dropped; the current one is applied to the new list.
  let list = [item("a")];
  const apply = (token, translations) => {
    if (gate.isCurrent(token)) list = mergeTranslations(list, translations);
  };
  apply(first, { a: "旧搜索" });
  assert.equal(list[0].translatedDescription, undefined);
  apply(second, { a: "新搜索" });
  assert.equal(list[0].translatedDescription, "新搜索");
});

test("pendingTranslationIdsBySource groups untranslated ids per source", () => {
  const list = [
    { id: "a", source: "Modrinth" },
    { id: "a", source: "Modrinth" },
    { id: "b", source: "Modrinth", translatedDescription: "已译" },
    { id: "1", source: "CurseForge" },
    { id: "2" },
    { source: "CurseForge" },
  ];
  assert.deepEqual(pendingTranslationIdsBySource(list), {
    Modrinth: ["a"],
    CurseForge: ["1"],
  });
  assert.deepEqual(pendingTranslationIdsBySource([]), {});
});

test("mergeTranslationsForSource ignores items of other sources", () => {
  const list = [
    { id: "1", source: "Modrinth" },
    { id: "1", source: "CurseForge" },
  ];
  const merged = mergeTranslationsForSource(list, "CurseForge", { 1: "译" });
  assert.equal(merged[0].translatedDescription, undefined);
  assert.equal(merged[1].translatedDescription, "译");
  assert.equal(merged[0], list[0]);
  assert.equal(mergeTranslationsForSource(list, "Modrinth", {}), list);
});
