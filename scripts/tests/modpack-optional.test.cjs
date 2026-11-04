const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

// Transpile the TS module on the fly; it only imports a type.
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
  invertOptional,
  normalizeOptionalPicks,
  selectAllOptional,
  selectNoneOptional,
  toggleOptional,
} = load("src/utils/modpack-optional.ts");

const file = (key) => ({ key, path: `mods/${key}.jar`, name: `${key}.jar` });
const files = [file("a"), file("b"), file("c")];

test("select all and none", () => {
  assert.deepEqual(selectAllOptional(files), ["a", "b", "c"]);
  assert.deepEqual(selectNoneOptional(), []);
});

test("invert flips every file and keeps the list order", () => {
  assert.deepEqual(invertOptional(files, ["b"]), ["a", "c"]);
  assert.deepEqual(invertOptional(files, []), ["a", "b", "c"]);
  assert.deepEqual(invertOptional(files, ["a", "b", "c"]), []);
});

test("invert ignores picks that name no listed file", () => {
  assert.deepEqual(invertOptional(files, ["gone"]), ["a", "b", "c"]);
});

test("toggle adds or removes one key", () => {
  assert.deepEqual(toggleOptional(["a"], "b"), ["a", "b"]);
  assert.deepEqual(toggleOptional(["a", "b"], "a"), ["b"]);
});

test("normalizing drops unknown picks and orders by the list", () => {
  assert.deepEqual(normalizeOptionalPicks(files, ["c", "gone", "a"]), [
    "a",
    "c",
  ]);
});

test("a repeated key is listed once", () => {
  assert.deepEqual(selectAllOptional([file("a"), file("a")]), ["a"]);
});
