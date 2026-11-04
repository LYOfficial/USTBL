const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function read(file) {
  return fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
}

function load(file, requireModule) {
  const module = { exports: {} };
  new Function(
    "exports",
    "module",
    "require",
    "React",
    ts.transpileModule(read(file), {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.React,
      },
    }).outputText
  )(module.exports, module, requireModule, { createElement: () => null });
  return module.exports;
}

const choiceUtils = load("src/utils/modpack-update-choices.ts", () => ({}));
const lists = load("src/components/modpack-update-lists.tsx", (name) =>
  name === "react"
    ? {
        memo: (component) => component,
        useMemo: (fn) => fn(),
        useState: () => [],
      }
    : name === "@/utils/modpack-update-choices"
      ? choiceUtils
      : {}
);

const hunk = () => ({
  baseStart: 0,
  baseEnd: 1,
  base: ["a"],
  local: ["b"],
  remote: ["c"],
});
const props = (conflict, choices, extra = {}) => ({
  conflict,
  choices,
  primaryColor: "blue",
  onChoose: () => {},
  onReveal: () => {},
  ...extra,
});

test("a conflict re-renders only for its own answers", () => {
  const file = { path: "a.cfg", kind: "x", hunks: [] };
  const text = { path: "b.cfg", kind: "x", hunks: [hunk(), hunk()] };
  const equal = lists.conflictItemEqual;
  const onChoose = () => {};
  const onReveal = () => {};
  const base = { onChoose, onReveal };

  // Another conflict's answer does not touch this one.
  assert.equal(
    equal(
      props(file, { "b.cfg#0": "takeRemote" }, base),
      props(file, { "b.cfg#0": "keepLocal", "b.cfg#1": "takeRemote" }, base)
    ),
    true
  );
  assert.equal(
    equal(props(text, { "a.cfg": "takeRemote" }, base), props(text, {}, base)),
    true
  );
  // Its own answers, including a default turning explicit, are compared by value.
  assert.equal(
    equal(props(file, {}, base), props(file, { "a.cfg": "takeRemote" }, base)),
    true
  );
  assert.equal(
    equal(props(file, {}, base), props(file, { "a.cfg": "keepLocal" }, base)),
    false
  );
  assert.equal(
    equal(
      props(text, { "b.cfg#0": "takeRemote" }, base),
      props(text, { "b.cfg#0": "takeRemote", "b.cfg#1": "keepLocal" }, base)
    ),
    false
  );
  // A new conflict object or callback always re-renders.
  assert.equal(
    equal(props(file, {}, base), props({ ...file }, {}, base)),
    false
  );
  assert.equal(
    equal(
      props(file, {}, base),
      props(file, {}, { ...base, onChoose: () => {} })
    ),
    false
  );
});

test("collapsed diff files do not keep their diff mounted", () => {
  const diff = read("src/components/common/diff-view.tsx");
  assert.match(diff, /<Collapse[^>]*unmountOnExit/);
  assert.match(diff, /export const DiffHunks = React\.memo\(/);
});

test("the update dialog hands conflicts to memoized items with stable callbacks", () => {
  const modal = read("src/components/modals/update-modpack-modal.tsx");
  assert.match(modal, /<ConflictItem[^>]*onChoose=\{chooseOne\}/s);
  assert.match(
    modal,
    /const chooseOne = useCallback\([\s\S]*?setChoices\(\(prev\)[\s\S]*?\[\]\s*\)/
  );
  // Choosing all is one state update, not one per conflict.
  const chooseAll = modal.slice(
    modal.indexOf("const chooseAll"),
    modal.indexOf("const chooseOne")
  );
  assert.equal(chooseAll.match(/setChoices\(/g).length, 1);
});

test("a conflict without text blocks is one row, not a collapsible card", () => {
  const source = read("src/components/modpack-update-lists.tsx");
  const item = source.slice(source.indexOf("export const ConflictItem"));
  const wholeFile = item.slice(
    item.indexOf("conflict.hunks.length === 0"),
    item.indexOf("<DiffFile")
  );
  assert.ok(wholeFile.length > 0);
  assert.doesNotMatch(wholeFile, /DiffFile/);
  assert.match(wholeFile, /<ChoiceButtons/);
});

test("the file-level answer sets every block and shows none when they differ", () => {
  const source = read("src/components/modpack-update-lists.tsx");
  assert.match(source, /onPick=\{\(option\) => onChooseMany\(keys, option\)\}/);
  assert.match(
    source,
    /const keys = conflict\.hunks\.map\(\(_, index\) => hunkKey/
  );
  assert.match(source, /\? first\s*: null/);
  const modal = read("src/components/modals/update-modpack-modal.tsx");
  const chooseMany = modal.slice(
    modal.indexOf("const chooseMany"),
    modal.indexOf("const [expandSignal")
  );
  assert.equal(chooseMany.match(/setChoices\(/g).length, 1);
  assert.match(modal, /onChooseMany=\{chooseMany\}/);
});

test("the dialog can expand and collapse every conflict card", () => {
  const modal = read("src/components/modals/update-modpack-modal.tsx");
  // Two icon buttons, like an IDE's expand all / collapse all.
  assert.match(modal, /\[true, "expandAll", LuUnfoldVertical\]/);
  assert.match(modal, /\[false, "collapseAll", LuFoldVertical\]/);
  assert.match(modal, /onClick=\{\(\) => expandAll\(open\)\}/);
  assert.match(modal, /expandSignal=\{expandSignal\}/);
  // Cards still start open.
  assert.match(modal, /useState<DiffExpandSignal>\(\{\s*open: true,/);
  const diff = read("src/components/common/diff-view.tsx");
  assert.match(
    diff,
    /defaultIsOpen: expandSignal \? expandSignal\.open : defaultOpen/
  );
});

test("a changed expand signal re-renders a conflict item", () => {
  const file = { path: "a.cfg", kind: "x", hunks: [hunk()] };
  const a = { open: false, token: 1 };
  assert.equal(
    lists.conflictItemEqual(
      props(file, {}, { expandSignal: a }),
      props(file, {}, { expandSignal: { ...a, token: 2 } })
    ),
    false
  );
});
