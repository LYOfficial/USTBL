const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");
const ts = require("typescript");
const source = fs.readFileSync(
  path.join(__dirname, "../../src/utils/outfit-draft.ts"),
  "utf8"
);
const exportsObject = {};
vm.runInNewContext(
  ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText,
  { exports: exportsObject }
);
const { outfitPreview } = exportsObject;
const current = {
  skin: "original-skin",
  cape: "original-cape",
  model: "classic",
};

test("skin and cape selections coexist regardless of the active tab", () => {
  const draft = { skin: { url: "new-skin", model: "slim" } };
  draft.cape = { url: "new-cape" };
  const preview = outfitPreview(draft, current);
  assert.equal(preview.skin, "new-skin");
  assert.equal(preview.cape, "new-cape");
  assert.equal(preview.model, "slim");
  draft.skin = { url: "another-skin", model: "classic" };
  assert.equal(outfitPreview(draft, current).cape, "new-cape");
});

test("clearing one texture preserves the other selected texture", () => {
  const preview = outfitPreview(
    { skin: { url: "new-skin", model: "slim" }, cape: null },
    current
  );
  assert.equal(preview.skin, "new-skin");
  assert.equal(preview.cape, undefined);
  const reset = outfitPreview({ skin: null }, current);
  assert.equal(reset.skin, undefined);
  assert.equal(reset.cape, "original-cape");
});

test("an empty draft retains existing textures and also supports new profiles", () => {
  assert.equal(outfitPreview({}, current).skin, "original-skin");
  assert.equal(outfitPreview({}, {}).skin, undefined);
  assert.equal(outfitPreview({}, {}).cape, undefined);
});
