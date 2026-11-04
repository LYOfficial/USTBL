const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function load(file) {
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
  )(module.exports, module, () => ({}), { createElement: () => null });
  return module.exports;
}

const change = load("src/components/modpack-version-change.tsx");

test("a version change is added, removed or changed by which side is missing", () => {
  assert.equal(change.versionChangeKind(null, "1.0"), "added");
  assert.equal(change.versionChangeKind(undefined, "1.0"), "added");
  assert.equal(change.versionChangeKind("1.0", null), "removed");
  assert.equal(change.versionChangeKind("1.0", "1.1"), "changed");
});

test("version changes use the colors of file changes", () => {
  assert.deepEqual(change.VERSION_CHANGE_COLOR, {
    added: "green",
    removed: "red",
    changed: "blue",
  });
  assert.equal(typeof change.VersionChangeRow, "function");
});

test("the update dialog draws component and loader changes as rows", () => {
  const modal = fs.readFileSync(
    path.join(
      __dirname,
      "../../src/components/modals/update-modpack-modal.tsx"
    ),
    "utf8"
  );
  assert.equal(modal.match(/<VersionChangeRow/g).length, 2);
  assert.doesNotMatch(modal, /componentChanges\.(added|removed|changed)"/);
});
