const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");
const ts = require("typescript");
const exportsObject = {};
vm.runInNewContext(
  ts.transpileModule(
    fs.readFileSync(
      path.join(__dirname, "../../src/utils/player-creation.ts"),
      "utf8"
    ),
    { compilerOptions: { module: ts.ModuleKind.CommonJS } }
  ).outputText,
  { exports: exportsObject }
);
const { playerCreationSource } = exportsObject;

for (const [source, expected] of [
  ["offline", "offline"],
  ["https://www.ustb.world/skinapi/", "vustb"],
  ["all", undefined],
  ["microsoft", undefined],
  ["https://other.example/skinapi/", undefined],
  ["vskin-library", undefined],
]) {
  test(`creation source whitelist: ${source}`, () => {
    assert.equal(playerCreationSource(source), expected);
  });
}
