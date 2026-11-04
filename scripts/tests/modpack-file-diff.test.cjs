const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

const read = (file) =>
  fs.readFileSync(path.join(__dirname, "../../", file), "utf8");

function load(file) {
  const module = { exports: {} };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(read(file), {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.React,
      },
    }).outputText
  )(module.exports, module, () => ({}));
  return module.exports;
}

const { fileDiffNotice } = load("src/components/modpack-file-diff.tsx");

test("a diff result is drawn, or replaced by the note that says why not", () => {
  const hunk = {
    oldStart: 1,
    oldLines: 1,
    newStart: 1,
    newLines: 1,
    lines: [],
  };
  assert.equal(fileDiffNotice({ hunks: [hunk], unavailable: null }), null);
  assert.equal(fileDiffNotice({ hunks: [], unavailable: null }), "formatOnly");
  assert.equal(
    fileDiffNotice({ hunks: null, unavailable: "notText" }),
    "notText"
  );
  assert.equal(
    fileDiffNotice({ hunks: null, unavailable: "unavailable" }),
    "unavailable"
  );
  assert.equal(
    fileDiffNotice({ hunks: null, unavailable: null }),
    "unavailable"
  );
});

test("only the update and auto-merged groups open diffs, for the shown plan", () => {
  const lists = read("src/components/modpack-update-lists.tsx");
  assert.equal(lists.match(/diff=\{diff\}/g).length, 2);
  assert.match(lists, /changes=\{groups\.update\}[^>]*?diff=\{diff\}/s);
  assert.match(lists, /changes=\{groups\.autoMerged\}[^>]*?diff=\{diff\}/s);
  assert.match(lists, /diff && canShowChangeDiff\(change\)/);
  assert.match(lists, /planHash: plan\.planHash/);
  const modal = read("src/components/modals/update-modpack-modal.tsx");
  assert.match(modal, /<PlanChangeGroups\s+instanceId=\{instanceId\}/);
});

test("the service calls the registered command with the plan hash", () => {
  const service = read("src/services/modpack-update.ts");
  assert.match(
    service,
    /invoke\("modpack_update_file_diff", \{\s*instanceId,\s*planHash,\s*path,?\s*\}\)/
  );
  assert.match(
    read("src-tauri/src/lib.rs"),
    /modpack_update::commands::modpack_update_file_diff/
  );
});
