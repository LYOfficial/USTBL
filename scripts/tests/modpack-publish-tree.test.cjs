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
      {
        compilerOptions: {
          module: ts.ModuleKind.CommonJS,
          target: ts.ScriptTarget.ES2017,
        },
      }
    ).outputText
  )(exports, module, () => ({}));
  return module.exports;
}

const {
  MAX_RULES,
  NO_FILTER,
  buildPublishNodes,
  canRemoveFile,
  categorizeChanges: categorizeChangesRaw,
  countCategories,
  defaultPublishProject,
  dirChangeCounts,
  filterNodes,
  isFilterActive,
  isSummaryEmpty,
  localDeletePaths,
  nextState,
  publishTargetAfterLoad,
  restoreInheritance,
  setDirState,
  setFileState,
  setOnce,
  summarizePublish,
  toPublishPayload,
  toggleCategory,
  viewOf,
} = load("src/utils/modpack-publish-tree.ts");

const categorizeChanges = (changes, ghosts, previous, view, context) =>
  categorizeChangesRaw(
    changes,
    ghosts,
    previous,
    view,
    new Map((context?.files ?? []).map((file) => [file.path.toLowerCase(), file.disabled]))
  );

// Plan 《整合包发布三态与本地忽略》§3-§4 is the source of every expectation.
// The starting state (§4 defaults) is worked out by the launcher and tested
// in `src-tauri/src/instance/modpack_publish/defaults.rs`; the cases here
// build the context and draft it hands over directly.

const rule = (p, kind, state) => ({ path: p, kind, state });
/** Files on disk: a plain path, or a `DiskFile` with its flags. */
const diskFile = (file) =>
  typeof file === "string"
    ? { path: file, disabled: false, conflict: false }
    : { disabled: false, conflict: false, ...file };
const ctxOf = (files, ghosts = []) => ({
  files: files.map(diskFile),
  ghosts,
});
const ghostOf = (p, kind = "file") => ({ path: p, kind });
const draftOf = (rules = [], { included = [], once = [] } = {}) => ({
  rules,
  included: new Set(included),
  once: new Set(once),
});
const paths = (payload) => payload.files.map((f) => f.path).sort();

// ---------------------------------------------------------------- virtual nodes

test("virtual file cannot be ticked", () => {
  const context = ctxOf(["a/keep.txt"], [ghostOf("a/old.txt")]);
  const next = setFileState(context, draftOf(), "a/old.txt", "include");
  assert.equal(viewOf(context, next).fileState("a/old.txt"), "remove");
  assert.deepEqual(paths(toPublishPayload(context, next)), ["a/keep.txt"]);
  assert.deepEqual(next.rules, []);
  assert.equal(next.included.size, 0);
});

test("virtual file can be set to ignore or delete (file rule) and back to remove", () => {
  const context = ctxOf(["a/keep.txt"], [ghostOf("a/old.txt")]);
  const ignored = setFileState(context, draftOf(), "a/old.txt", "ignore");
  assert.deepEqual(ignored.rules, [rule("a/old.txt", "file", "ignore")]);
  assert.equal(viewOf(context, ignored).fileState("a/old.txt"), "ignore");

  const deleted = setFileState(context, ignored, "a/old.txt", "delete");
  assert.deepEqual(deleted.rules, [rule("a/old.txt", "file", "delete")]);

  const removed = setFileState(context, deleted, "a/old.txt", "remove");
  assert.deepEqual(removed.rules, []);
  assert.equal(viewOf(context, removed).fileState("a/old.txt"), "remove");

  const restored = restoreInheritance(deleted, "a/old.txt", "file");
  assert.deepEqual(restored.rules, []);
});

test("virtual file: remove is only offered when no dir rule covers it", () => {
  assert.equal(canRemoveFile(draftOf([]), "a/old.txt"), true);
  assert.equal(canRemoveFile(draftOf([rule("a", "dir", "ignore")]), "a/old.txt"), false);
  assert.equal(canRemoveFile(draftOf([rule("A", "dir", "delete")]), "a/b/old.txt"), false);
  assert.equal(canRemoveFile(draftOf([rule("a/old.txt", "file", "ignore")]), "a/old.txt"), true);
});

test("virtual node for a previous rule whose target is gone", () => {
  const context = ctxOf(["config/a.toml"], [ghostOf("gone.txt"), ghostOf("gonedir", "dir")]);
  assert.deepEqual(
    context.ghosts.slice().sort((a, b) => a.path.localeCompare(b.path)),
    [
      { path: "gone.txt", kind: "file" },
      { path: "gonedir", kind: "dir" },
    ]
  );
  const nodes = buildPublishNodes(context);
  const gonedir = nodes.find((n) => n.path === "gonedir");
  assert.ok(gonedir);
  assert.equal(gonedir.ghost, true);
  assert.equal(gonedir.kind, "dir");
});

test("buildPublishNodes: ghost flag only on missing files, real parents are not ghosts", () => {
  const [dir] = buildPublishNodes(ctxOf(["a/keep.txt"], [ghostOf("a/old.txt")]));
  assert.equal(dir.path, "a");
  assert.equal(dir.ghost, false);
  const byName = Object.fromEntries(dir.children.map((c) => [c.name, c]));
  assert.equal(byName["keep.txt"].ghost, false);
  assert.equal(byName["old.txt"].ghost, true);
});

test("nextState cycles real files include > ignore > delete and ghosts remove > ignore > delete", () => {
  const real = { canRemove: false };
  assert.equal(nextState("include", real), "ignore");
  assert.equal(nextState("ignore", real), "delete");
  assert.equal(nextState("delete", real), "include");
  assert.equal(nextState("mixed", real), "include");
  const ghost = { canRemove: true };
  assert.equal(nextState("remove", ghost), "ignore");
  assert.equal(nextState("ignore", ghost), "delete");
  assert.equal(nextState("delete", ghost), "remove");
});

// ---------------------------------------------------------------- rule priority

test("priority: file rule > deeper dir rule > shallower dir rule > unmentioned", () => {
  const context = ctxOf(["a/b/c/x.txt", "a/b/c/y.txt", "a/b/z.txt", "a/w.txt", "top.txt"]);
  const draft = draftOf([
    rule("a", "dir", "ignore"),
    rule("a/b", "dir", "delete"),
    rule("a/b/c/y.txt", "file", "ignore"),
  ]);
  const v = viewOf(context, draft);
  assert.equal(v.fileState("a/b/c/y.txt"), "ignore");
  assert.equal(v.fileState("a/b/c/x.txt"), "delete");
  assert.equal(v.fileState("a/b/z.txt"), "delete");
  assert.equal(v.fileState("a/w.txt"), "ignore");
  assert.equal(v.fileState("top.txt"), "include");
});

test("priority: the deeper dir rule wins regardless of rule order", () => {
  const context = ctxOf(["a/b/x.txt"]);
  const deepFirst = viewOf(context, draftOf([rule("a/b", "dir", "delete"), rule("a", "dir", "ignore")]));
  const shallowFirst = viewOf(context, draftOf([rule("a", "dir", "ignore"), rule("a/b", "dir", "delete")]));
  assert.equal(deepFirst.fileState("a/b/x.txt"), "delete");
  assert.equal(shallowFirst.fileState("a/b/x.txt"), "delete");
});

test("priority: an explicit tick (files entry) beats a covering dir rule and a file rule", () => {
  const context = ctxOf(["a/x.txt", "a/y.txt"]);
  const v = viewOf(
    context,
    draftOf([rule("a", "dir", "delete")], { included: ["a/x.txt"] })
  );
  assert.equal(v.fileState("a/x.txt"), "include");
  assert.equal(v.fileState("a/y.txt"), "delete");
});

test("a dir rule does not match the directory's own name as a file, nor a sibling with the same prefix", () => {
  const context = ctxOf(["config", "config2/a.toml"]);
  const v = viewOf(context, draftOf([rule("config", "dir", "ignore")]));
  assert.equal(v.fileState("config"), "include");
  assert.equal(v.fileState("config2/a.toml"), "include");
});

test("rule matching is case-insensitive", () => {
  const context = ctxOf(["Config/Sub/X.toml"]);
  const v = viewOf(context, draftOf([rule("config", "dir", "ignore")]));
  assert.equal(v.fileState("Config/Sub/X.toml"), "ignore");
  const v2 = viewOf(context, draftOf([rule("CONFIG/sub/x.TOML", "file", "delete")]));
  assert.equal(v2.fileState("Config/Sub/X.toml"), "delete");
});

test("dirState: uniform children show that state, different children show mixed", () => {
  const context = ctxOf(["a/x.txt", "a/y.txt", "b/z.txt"]);
  const v = viewOf(context, draftOf([rule("a/y.txt", "file", "ignore")]));
  assert.equal(v.dirState("a"), "mixed");
  assert.equal(v.dirState("b"), "include");
  const allIgnored = viewOf(context, draftOf([rule("a", "dir", "ignore")]));
  assert.equal(allIgnored.dirState("a"), "ignore");
});

// ---------------------------------------------------------------- setting states

test("setFileState: a file in a plain tree gets exactly one file rule, replaced on the next change", () => {
  const context = ctxOf(["a/x.txt", "a/y.txt"]);
  const ignored = setFileState(context, draftOf(), "a/x.txt", "ignore");
  assert.deepEqual(ignored.rules, [rule("a/x.txt", "file", "ignore")]);
  const deleted = setFileState(context, ignored, "a/x.txt", "delete");
  assert.deepEqual(deleted.rules, [rule("a/x.txt", "file", "delete")]);
  const back = setFileState(context, deleted, "a/x.txt", "include");
  assert.deepEqual(back.rules, []);
  assert.equal(back.included.size, 0);
});

test("setFileState: a state equal to what the file inherits adds no rule", () => {
  const context = ctxOf(["a/x.txt"]);
  const draft = draftOf([rule("a", "dir", "ignore")]);
  const next = setFileState(context, draft, "a/x.txt", "ignore");
  assert.deepEqual(next.rules, [rule("a", "dir", "ignore")]);
});

test("setFileState: a different state under a dir rule adds a file rule that wins", () => {
  const context = ctxOf(["a/x.txt", "a/y.txt"]);
  const draft = draftOf([rule("a", "dir", "ignore")]);
  const next = setFileState(context, draft, "a/x.txt", "delete");
  assert.deepEqual(next.rules, [rule("a", "dir", "ignore"), rule("a/x.txt", "file", "delete")]);
  const v = viewOf(context, next);
  assert.equal(v.fileState("a/x.txt"), "delete");
  assert.equal(v.fileState("a/y.txt"), "ignore");
});

test("setFileState: ticking a file under an ignore dir publishes it without any rule change", () => {
  const context = ctxOf(["a/x.txt", "a/y.txt"]);
  const draft = draftOf([rule("a", "dir", "ignore")]);
  const next = setFileState(context, draft, "a/x.txt", "include");
  assert.deepEqual(next.rules, [rule("a", "dir", "ignore")]);
  const payload = toPublishPayload(context, next);
  assert.deepEqual(paths(payload), ["a/x.txt"]);
});

test("setFileState: ticking a file that has its own file rule drops that rule", () => {
  const context = ctxOf(["a/x.txt"]);
  const draft = draftOf([rule("a/x.txt", "file", "ignore")]);
  const next = setFileState(context, draft, "a/x.txt", "include");
  assert.deepEqual(next.rules, []);
  assert.equal(viewOf(context, next).fileState("a/x.txt"), "include");
});

test("setFileState does not mutate its input draft", () => {
  const context = ctxOf(["a/x.txt"]);
  const draft = draftOf([rule("a", "dir", "ignore")]);
  setFileState(context, draft, "a/x.txt", "include");
  setFileState(context, draft, "a/x.txt", "delete");
  assert.deepEqual(draft.rules, [rule("a", "dir", "ignore")]);
  assert.equal(draft.included.size, 0);
});

test("setDirState: ignore or delete on a plain dir leaves one rule on that dir", () => {
  const context = ctxOf(["a/x.txt", "a/b/y.txt", "c/z.txt"]);
  const next = setDirState(context, draftOf(), "a", "ignore");
  assert.deepEqual(next.rules, [rule("a", "dir", "ignore")]);
  const v = viewOf(context, next);
  assert.equal(v.fileState("a/b/y.txt"), "ignore");
  assert.equal(v.fileState("c/z.txt"), "include");
  const del = setDirState(context, next, "a", "delete");
  assert.deepEqual(del.rules, [rule("a", "dir", "delete")]);
});

test("setDirState: a state equal to the inherited one adds no rule", () => {
  const context = ctxOf(["a/b/x.txt"]);
  const next = setDirState(context, draftOf([rule("a", "dir", "ignore")]), "a/b", "ignore");
  assert.deepEqual(next.rules, [rule("a", "dir", "ignore")]);
  const plain = setDirState(context, draftOf([]), "a", "include");
  assert.deepEqual(plain.rules, []);
});

test("setDirState: ticking a subdir under an ignore dir only ticks the files there now (§4)", () => {
  const context = ctxOf(["saves/w1/a.dat", "saves/w1/b.dat", "saves/w2/c.dat"]);
  const draft = draftOf([rule("saves", "dir", "ignore")]);
  const next = setDirState(context, draft, "saves/w1", "include");
  assert.deepEqual(next.rules, [rule("saves", "dir", "ignore")]);
  assert.deepEqual(paths(toPublishPayload(context, next)), ["saves/w1/a.dat", "saves/w1/b.dat"]);

  const later = ctxOf([
    "saves/w1/a.dat",
    "saves/w1/b.dat",
    "saves/w1/new.dat",
    "saves/w2/c.dat",
  ]);
  const v = viewOf(later, next);
  assert.equal(v.fileState("saves/w1/new.dat"), "ignore");
  assert.deepEqual(paths(toPublishPayload(later, next)), ["saves/w1/a.dat", "saves/w1/b.dat"]);
});

test("setDirState: ticking a dir that has its own rule removes that rule", () => {
  const context = ctxOf(["a/x.txt", "a/y.txt"]);
  const next = setDirState(context, draftOf([rule("a", "dir", "ignore")]), "a", "include");
  assert.deepEqual(next.rules, []);
  assert.deepEqual(paths(toPublishPayload(context, next)), ["a/x.txt", "a/y.txt"]);
});

test("rules stay unique per casefolded path when set with different casing", () => {
  const context = ctxOf(["Config/A.toml", "Config/B.toml"]);
  const f = setFileState(
    context,
    draftOf([rule("Config/A.toml", "file", "ignore")]),
    "config/a.toml",
    "delete"
  );
  assert.equal(f.rules.length, 1);
  assert.equal(f.rules[0].state, "delete");

  const d = setDirState(context, draftOf([rule("config", "dir", "ignore")]), "CONFIG", "delete");
  assert.equal(d.rules.length, 1);
  assert.equal(d.rules[0].state, "delete");
});

// ---------------------------------------------------------------- no compression

test("payload rules are exactly the draft rules, in order, redundant ones included", () => {
  const context = ctxOf(["a/b/x.txt", "a/y.txt"]);
  const rules = [
    rule("a/b", "dir", "ignore"),
    rule("a", "dir", "ignore"),
    rule("a/y.txt", "file", "ignore"),
    rule("a/b/x.txt", "file", "ignore"),
  ];
  const payload = toPublishPayload(context, draftOf(rules));
  assert.deepEqual(payload.rules, rules);
  assert.deepEqual(payload.files, []);
});

test("per-file rules are not folded into a dir rule and a dir rule is not expanded to files", () => {
  const context = ctxOf(["a/x.txt", "a/y.txt"]);
  const perFile = [rule("a/x.txt", "file", "ignore"), rule("a/y.txt", "file", "ignore")];
  assert.deepEqual(toPublishPayload(context, draftOf(perFile)).rules, perFile);
  const dir = [rule("a", "dir", "ignore")];
  assert.deepEqual(toPublishPayload(context, draftOf(dir)).rules, dir);
});

test("payload rules are copies, not the draft's own objects", () => {
  const context = ctxOf(["a/x.txt"]);
  const draft = draftOf([rule("a", "dir", "ignore")]);
  const payload = toPublishPayload(context, draft);
  payload.rules[0].state = "delete";
  assert.equal(draft.rules[0].state, "ignore");
});

test("grey and red files never reach the payload files, only ticked ones do", () => {
  const context = ctxOf(["a.txt", "b.txt", "c.txt"]);
  const payload = toPublishPayload(
    context,
    draftOf([rule("b.txt", "file", "ignore"), rule("c.txt", "file", "delete")])
  );
  assert.deepEqual(paths(payload), ["a.txt"]);
});

// ---------------------------------------------------------------- once

test("once is carried on ticked files in the payload", () => {
  const context = ctxOf(["config/a.toml", "config/b.toml"]);
  const draft = setOnce(draftOf([]), "config/a.toml", true);
  const payload = toPublishPayload(context, draft);
  assert.deepEqual(
    payload.files.map((f) => [f.path, f.once]).sort(),
    [
      ["config/a.toml", true],
      ["config/b.toml", false],
    ]
  );
  const off = setOnce(draft, "config/a.toml", false);
  assert.ok(toPublishPayload(context, off).files.every((f) => !f.once));
});

test("once is not published for a file that is not ticked", () => {
  const context = ctxOf(["config/a.toml"]);
  const draft = setOnce(draftOf([rule("config/a.toml", "file", "ignore")]), "config/a.toml", true);
  const payload = toPublishPayload(context, draft);
  assert.deepEqual(payload.files, []);
});

test("once only exists on files: a directory path never marks its files once", () => {
  const context = ctxOf(["config/a.toml", "config/b.toml"]);
  const draft = setOnce(draftOf([]), "config", true);
  const payload = toPublishPayload(context, draft);
  assert.ok(payload.files.every((f) => f.once === false));
  const v = viewOf(context, draft);
  assert.equal(v.isOnce("config/a.toml"), false);
});

test("setOnce does not mutate its input draft and lookups are case-insensitive", () => {
  const draft = draftOf([]);
  const next = setOnce(draft, "Config/A.toml", true);
  assert.equal(draft.once.size, 0);
  assert.equal(viewOf(ctxOf(["config/a.toml"]), next).isOnce("config/a.toml"), true);
});

// ---------------------------------------------------------------- restore inheritance

test("restore inheritance on a file removes its file rule so it follows the covering dir rule", () => {
  const context = ctxOf(["a/x.txt"]);
  const draft = draftOf([rule("a", "dir", "ignore"), rule("a/x.txt", "file", "delete")]);
  const next = restoreInheritance(draft, "a/x.txt", "file");
  assert.deepEqual(next.rules, [rule("a", "dir", "ignore")]);
  assert.equal(viewOf(context, next).fileState("a/x.txt"), "ignore");
});

test("restore inheritance on a ticked file under a dir rule removes the tick", () => {
  const context = ctxOf(["a/x.txt"]);
  const draft = draftOf([rule("a", "dir", "ignore")], { included: ["a/x.txt"] });
  const next = restoreInheritance(draft, "a/x.txt", "file");
  assert.equal(next.included.size, 0);
  assert.equal(viewOf(context, next).fileState("a/x.txt"), "ignore");
  assert.deepEqual(next.rules, [rule("a", "dir", "ignore")]);
});

test("restore inheritance on a dir removes that dir's rule and keeps the others", () => {
  const draft = draftOf([
    rule("a", "dir", "ignore"),
    rule("a/b", "dir", "delete"),
    rule("c", "dir", "ignore"),
  ]);
  const next = restoreInheritance(draft, "a/b", "dir");
  assert.deepEqual(next.rules, [rule("a", "dir", "ignore"), rule("c", "dir", "ignore")]);
  const top = restoreInheritance(draft, "a", "dir");
  assert.ok(!top.rules.some((r) => r.path === "a"));
  assert.ok(top.rules.some((r) => r.path === "c"));
});

test("restore inheritance matches paths case-insensitively and does not touch the input", () => {
  const draft = draftOf([rule("Config", "dir", "ignore")]);
  const next = restoreInheritance(draft, "config", "dir");
  assert.deepEqual(next.rules, []);
  assert.equal(draft.rules.length, 1);
});

test("hasOverride is true on a node with its own rule or tick, false when it only inherits", () => {
  const context = ctxOf(["a/x.txt", "a/y.txt", "a/z.txt"]);
  const draft = draftOf(
    [rule("a", "dir", "ignore"), rule("a/x.txt", "file", "delete")],
    { included: ["a/y.txt"] }
  );
  const v = viewOf(context, draft);
  assert.equal(v.hasOverride("a", "dir"), true);
  assert.equal(v.hasOverride("a/x.txt", "file"), true);
  assert.equal(v.hasOverride("a/y.txt", "file"), true);
  assert.equal(v.hasOverride("a/z.txt", "file"), false);
});

// ---------------------------------------------------------------- disabled mods

test("both variants on disk: the conflict is reported for the ticked file only", () => {
  const context = ctxOf([{ path: "mods/a.jar", conflict: true }, "mods/b.jar"]);
  assert.deepEqual(toPublishPayload(context, draftOf()).conflicts, ["mods/a.jar"]);
  const ignored = draftOf([rule("mods/a.jar", "file", "ignore")]);
  assert.deepEqual(toPublishPayload(context, ignored).conflicts, []);
});

test("buildPublishNodes marks a disabled mod under its enabled name", () => {
  const context = ctxOf([
    { path: "mods/a.jar", disabled: true },
    "mods/b.jar",
    { path: "mods/c.jar", conflict: true },
  ]);
  const mods = buildPublishNodes(context).find((n) => n.path === "mods");
  const byName = Object.fromEntries(mods.children.map((c) => [c.name, c]));
  assert.equal(byName["a.jar"].disabled, true);
  assert.equal(byName["b.jar"].disabled, false);
  assert.equal(byName["c.jar"].conflict, true);
  assert.equal(byName["a.jar.disabled"], undefined);
});

test("buildPublishNodes lists directories before files", () => {
  const nodes = buildPublishNodes(ctxOf(["z.txt", "a/x.txt", "b.txt"]));
  assert.deepEqual(nodes.map((n) => n.name), ["a", "b.txt", "z.txt"]);
});

// ---------------------------------------------------------------- casefold

test("file state lookups ignore the case of the queried path", () => {
  const context = ctxOf(["Config/A.toml"]);
  const v = viewOf(context, draftOf([rule("config/a.toml", "file", "ignore")]));
  assert.equal(v.fileState("CONFIG/A.TOML"), "ignore");
});

// ---------------------------------------------------------------- limits

test("MAX_RULES matches the manifest limit of §3 (2000)", () => {
  assert.equal(MAX_RULES, 2000);
});

test("the payload carries every rule the draft holds, so the caller can compare against MAX_RULES", () => {
  const rules = Array.from({ length: MAX_RULES + 1 }, (_, i) => rule(`d/f${i}.txt`, "file", "ignore"));
  const payload = toPublishPayload(ctxOf(["a.txt"]), draftOf(rules));
  assert.equal(payload.rules.length, MAX_RULES + 1);
});


// ------------------------------------------- directory settings keep the subtree

test("cycling a directory through every state keeps the rules set below it", () => {
  const context = ctxOf(["config/a.toml", "config/cache/b.bin", "mods/x.jar"]);
  const original = [rule("config/cache", "dir", "ignore")];
  let draft = draftOf(original);
  const seen = [];
  for (const next of ["ignore", "delete", "include"]) {
    draft = setDirState(context, draft, "config", next);
    seen.push(draft.rules.map((r) => `${r.path}:${r.state}`));
    assert.ok(draft.rules.some((r) => r.path === "config/cache" && r.state === "ignore"));
    assert.equal(viewOf(context, draft).fileState("config/cache/b.bin"), "ignore");
  }
  assert.deepEqual(seen, [
    ["config/cache:ignore", "config:ignore"],
    ["config/cache:ignore", "config:delete"],
    ["config/cache:ignore"],
  ]);
  assert.deepEqual(draft.rules, original);
  assert.deepEqual(paths(toPublishPayload(context, draft)), ["config/a.toml", "mods/x.jar"]);
});

test("a directory with an overriding child shows mixed until the child inherits again", () => {
  const context = ctxOf(["config/a.toml", "config/cache/b.bin"]);
  const draft = setDirState(context, draftOf([rule("config/cache", "dir", "ignore")]), "config", "include");
  assert.equal(viewOf(context, draft).dirState("config"), "mixed");
  const restored = restoreInheritance(draft, "config/cache", "dir");
  assert.equal(viewOf(context, restored).dirState("config"), "include");
});

test("ticking a directory leaves its explicit child settings alone", () => {
  const context = ctxOf([
    "saves/w1/a.dat",
    "saves/w1/keep.dat",
    "saves/w1/deep/b.dat",
    "saves/w1/deep/c.dat",
    "saves/w2/d.dat",
  ]);
  const draft = draftOf([
    rule("saves", "dir", "ignore"),
    rule("saves/w1/keep.dat", "file", "delete"),
    rule("saves/w1/deep", "dir", "delete"),
  ]);
  const next = setDirState(context, draft, "saves/w1", "include");
  assert.deepEqual(next.rules, draft.rules);
  const v = viewOf(context, next);
  assert.equal(v.fileState("saves/w1/a.dat"), "include");
  assert.equal(v.fileState("saves/w1/keep.dat"), "delete");
  assert.equal(v.fileState("saves/w1/deep/b.dat"), "delete");
  assert.equal(v.fileState("saves/w2/d.dat"), "ignore");
  assert.deepEqual(paths(toPublishPayload(context, next)), ["saves/w1/a.dat"]);
});

test("ignoring a directory keeps the ticks and rules below it", () => {
  const context = ctxOf(["a/x.txt", "a/b/y.txt", "a/b/z.txt"]);
  const draft = draftOf([rule("a/b", "dir", "delete")], { included: ["a/b/z.txt"] });
  const next = setDirState(context, draft, "a", "ignore");
  assert.deepEqual(next.rules, [rule("a/b", "dir", "delete"), rule("a", "dir", "ignore")]);
  const v = viewOf(context, next);
  assert.equal(v.fileState("a/x.txt"), "ignore");
  assert.equal(v.fileState("a/b/y.txt"), "delete");
  assert.equal(v.fileState("a/b/z.txt"), "include");
});

test("changing a directory's rule state replaces it in place", () => {
  const context = ctxOf(["a/x.txt", "b/y.txt"]);
  const draft = draftOf([rule("a", "dir", "ignore"), rule("b", "dir", "ignore")]);
  const next = setDirState(context, draft, "a", "delete");
  assert.deepEqual(next.rules, [rule("a", "dir", "delete"), rule("b", "dir", "ignore")]);
});

// ------------------------------------------------- virtual nodes never get ticked

test("a virtual file covered by a directory rule cycles only ignore and delete", () => {
  const ghost = { canRemove: false, ghost: true };
  assert.equal(nextState("ignore", ghost), "delete");
  assert.equal(nextState("delete", ghost), "ignore");
  assert.equal(nextState("remove", ghost), "ignore");
  const context = ctxOf([], [ghostOf("cache/old.bin")]);
  let draft = draftOf([rule("cache", "dir", "ignore")]);
  assert.equal(canRemoveFile(draft, "cache/old.bin"), false);
  for (let i = 0; i < 5; i++) {
    const current = viewOf(context, draft).fileState("cache/old.bin");
    draft = setFileState(context, draft, "cache/old.bin", nextState(current, ghost));
    const after = viewOf(context, draft).fileState("cache/old.bin");
    assert.ok(after === "ignore" || after === "delete");
  }
  assert.deepEqual(toPublishPayload(context, draft).files, []);
});

test("a virtual directory cycles remove, ignore, delete and is not ticked", () => {
  const context = ctxOf(["a.txt"], [ghostOf("gone", "dir")]);
  const draft = draftOf([rule("gone", "dir", "delete")]);
  assert.deepEqual(context.ghosts, [{ path: "gone", kind: "dir" }]);
  assert.equal(viewOf(context, draft).dirState("gone"), "delete");
  const restored = restoreInheritance(draft, "gone", "dir");
  assert.deepEqual(restored.rules, []);
  assert.equal(viewOf(context, restored).dirState("gone"), "remove");
  const nodes = buildPublishNodes(context);
  assert.ok(nodes.some((node) => node.name === "gone" && node.kind === "dir" && node.ghost));
});

// ---------------------------------------------------------------- misc

test("a rule with nothing ticked is still a publishable payload", () => {
  const context = ctxOf(["a.txt"]);
  const draft = setFileState(context, draftOf([]), "a.txt", "delete");
  const payload = toPublishPayload(context, draft);
  assert.deepEqual(payload.files, []);
  assert.deepEqual(payload.rules, [rule("a.txt", "file", "delete")]);
});

// ------------------------------------------------------- default project (H2)

test("the project the instance came from is the default when it can be published to", () => {
  const projects = [{ slug: "gtnh" }, { slug: "other" }];
  const origin = { platform: "campus", projectId: "gtnh" };
  assert.equal(defaultPublishProject(origin, projects), "gtnh");
});

test("an origin whose project is not in the list defaults to a new project", () => {
  const origin = { platform: "campus", projectId: "gone" };
  assert.equal(defaultPublishProject(origin, [{ slug: "gtnh" }]), null);
  assert.equal(defaultPublishProject(origin, []), null);
});

test("an instance from another platform or without an origin defaults to a new project", () => {
  const projects = [{ slug: "gtnh" }];
  // The same id on Modrinth is another project.
  assert.equal(
    defaultPublishProject({ platform: "modrinth", projectId: "gtnh" }, projects),
    null
  );
  assert.equal(
    defaultPublishProject({ platform: "curseforge", projectId: "gtnh" }, projects),
    null
  );
  assert.equal(defaultPublishProject(undefined, projects), null);
  assert.equal(defaultPublishProject(null, projects), null);
});

test("the default does not look at whether the instance still follows the project", () => {
  // `follow` is not even an input: an unfollowed instance publishes back.
  const origin = { platform: "campus", projectId: "gtnh", follow: false };
  assert.equal(defaultPublishProject(origin, [{ slug: "gtnh" }]), "gtnh");
});

test("a target the user picked is kept when the list arrives", () => {
  const origin = { platform: "campus", projectId: "gtnh" };
  const projects = [{ slug: "gtnh" }, { slug: "other" }];
  assert.equal(publishTargetAfterLoad(undefined, origin, projects), "gtnh");
  assert.equal(publishTargetAfterLoad("other", origin, projects), "other");
  // Choosing "new project" is a choice too.
  assert.equal(publishTargetAfterLoad(null, origin, projects), null);
});

// --------------------------------------------------------------- change marks

const change = (p, kind, extra = {}) => ({
  path: p,
  kind,
  previousSha1: kind === "added" ? null : "aa",
  previousSize: kind === "added" ? null : 1,
  localSize: 2,
  localIgnored: false,
  ...extra,
});
const before = (p, extra = {}) => ({
  path: p,
  sha1: "aa",
  size: 1,
  disabled: false,
  once: false,
  ...extra,
});
const release = (files, rules = []) => ({ files, rules, rulesManifest: true });

test("every mark is by lowercased path and a missing previous file is deleted", () => {
  const ghosts = [ghostOf("mods/Gone.jar"), ghostOf("cache", "dir"), ghostOf("old/rule.txt")];
  const previous = release([before("mods/gone.jar"), before("mods/A.jar")]);
  const marks = categorizeChanges(
    [change("Mods/A.jar", "modified"), change("mods/new.jar", "added")],
    ghosts,
    previous,
    viewOf(ctxOf(["mods/A.jar", "mods/new.jar"]), draftOf())
  );
  assert.equal(marks.get("mods/a.jar"), "modified");
  assert.equal(marks.get("mods/new.jar"), "added");
  assert.equal(marks.get("mods/gone.jar"), "deleted");
  // A rule's target and a directory are not deleted files.
  assert.equal(marks.has("cache"), false);
  assert.equal(marks.has("old/rule.txt"), false);
  assert.deepEqual(countCategories(marks), {
    modified: 1,
    added: 1,
    enable: 0,
    disable: 0,
    onceChanged: 0,
    deleted: 1,
  });
});

test("a new project has no deleted files", () => {
  const marks = categorizeChanges(
    [change("a.txt", "added")],
    [ghostOf("b.txt")],
    null,
    viewOf(ctxOf(["a.txt"]), draftOf())
  );
  assert.deepEqual(Array.from(marks.keys()), ["a.txt"]);
});

test("a directory counts the marked files below it at every level", () => {
  const marks = categorizeChanges(
    [
      change("mods/a.jar", "modified"),
      change("mods/sub/b.jar", "added"),
      change("config/c.cfg", "toggled"),
      change("root.txt", "added"),
    ],
    [],
    null,
    viewOf(
      ctxOf(["mods/a.jar", "mods/sub/b.jar", "config/c.cfg", "root.txt"]),
      draftOf()
    )
  );
  const counts = dirChangeCounts(marks);
  assert.equal(counts.get("mods"), 2);
  assert.equal(counts.get("mods/sub"), 1);
  assert.equal(counts.get("config"), 1);
  assert.equal(counts.has("root.txt"), false);
});

test("a new file only counts as added while the draft publishes it", () => {
  const context = ctxOf(["saves/w/r.mca", "mods/new.jar", "mods/a.jar"]);
  const changes = [
    change("saves/w/r.mca", "added"),
    change("mods/new.jar", "added"),
    change("mods/a.jar", "modified"),
  ];
  const previous = release([before("mods/a.jar")]);
  const draft = draftOf([rule("saves", "dir", "ignore")]);
  const marksOf = (d) =>
    categorizeChanges(changes, [], previous, viewOf(context, d));

  let marks = marksOf(draft);
  assert.equal(marks.has("saves/w/r.mca"), false);
  assert.equal(marks.get("mods/new.jar"), "added");
  assert.equal(countCategories(marks).added, 1);
  assert.equal(countCategories(marks).modified, 1);

  // Publishing the ignored file brings it into "added"; ignoring it again removes it.
  const published = setFileState(context, draft, "saves/w/r.mca", "include");
  marks = marksOf(published);
  assert.equal(marks.get("saves/w/r.mca"), "added");
  assert.equal(countCategories(marks).added, 2);
  assert.equal(dirChangeCounts(marks).get("saves/w"), 1);
  const back = setFileState(context, published, "saves/w/r.mca", "ignore");
  assert.equal(marksOf(back).has("saves/w/r.mca"), false);
});

test("a new file under the previous release's ignore rule is added only once published", () => {
  const context = ctxOf(["config/x.cfg"]);
  const changes = [change("config/x.cfg", "added")];
  const previous = release([], [rule("config", "dir", "ignore")]);
  const covered = draftOf(previous.rules);
  assert.equal(
    categorizeChanges(changes, [], previous, viewOf(context, covered)).has(
      "config/x.cfg"
    ),
    false
  );
  const published = setFileState(context, covered, "config/x.cfg", "include");
  assert.equal(
    categorizeChanges(changes, [], previous, viewOf(context, published)).get(
      "config/x.cfg"
    ),
    "added"
  );
});

test("a new project marks only the files it publishes as added", () => {
  const context = ctxOf(["a.txt", "saves/s.dat"]);
  const changes = [change("a.txt", "added"), change("saves/s.dat", "added")];
  const draft = draftOf([rule("saves", "dir", "ignore")]);
  const marks = categorizeChanges(changes, [], null, viewOf(context, draft));
  assert.deepEqual(Array.from(marks.keys()), ["a.txt"]);
});

const treeFixture = () => {
  const context = ctxOf(
    ["mods/a.jar", "mods/b.jar", "mods/sub/c.jar", "config/d.cfg", "keep/e.txt"],
    [ghostOf("mods/old.jar")]
  );
  const previous = release([before("mods/old.jar"), before("mods/a.jar")]);
  const marks = categorizeChanges(
    [
      change("mods/a.jar", "modified"),
      change("mods/sub/c.jar", "added"),
      change("config/d.cfg", "toggled"),
    ],
    context.ghosts,
    previous,
    viewOf(context, draftOf())
  );
  return { nodes: buildPublishNodes(context), marks };
};
const names = (nodes) =>
  nodes.flatMap((node) =>
    node.kind === "file" ? [node.path] : names(node.children)
  );

test("without a filter the tree is returned as is", () => {
  const { nodes, marks } = treeFixture();
  assert.equal(isFilterActive(NO_FILTER), false);
  assert.equal(filterNodes(nodes, marks, NO_FILTER), nodes);
});

test("only changed keeps marked files and the directories leading to them", () => {
  const { nodes, marks } = treeFixture();
  const shown = filterNodes(nodes, marks, { ...NO_FILTER, onlyChanged: true });
  assert.deepEqual(names(shown).sort(), [
    "config/d.cfg",
    "mods/a.jar",
    "mods/old.jar",
    "mods/sub/c.jar",
  ]);
  assert.deepEqual(shown.map((node) => node.name).sort(), ["config", "mods"]);
});

test("picking a mark shows only that mark and several marks add up", () => {
  const { nodes, marks } = treeFixture();
  let filter = toggleCategory(NO_FILTER, "added");
  assert.equal(isFilterActive(filter), true);
  assert.deepEqual(names(filterNodes(nodes, marks, filter)), ["mods/sub/c.jar"]);
  filter = toggleCategory(filter, "deleted");
  assert.deepEqual(names(filterNodes(nodes, marks, filter)).sort(), [
    "mods/old.jar",
    "mods/sub/c.jar",
  ]);
  filter = toggleCategory(toggleCategory(filter, "added"), "deleted");
  assert.equal(isFilterActive(filter), false);
  assert.deepEqual(NO_FILTER.categories.size, 0);
});

test("a filter that nothing matches leaves an empty tree", () => {
  const { nodes, marks } = treeFixture();
  const shown = filterNodes(nodes, marks, toggleCategory(NO_FILTER, "onceChanged"));
  assert.deepEqual(shown, []);
});

// ------------------------------------------------------------ publish summary

const sizes = (summary) =>
  Object.fromEntries(
    Object.entries(summary)
      .filter(([, value]) => Array.isArray(value) && value.length > 0)
      .map(([key, value]) => [key, value.length])
  );

test("a new project adds every pushed file and every rule", () => {
  const context = ctxOf(["a.txt", "b/c.txt", "cache/x.bin"]);
  const draft = draftOf([rule("cache", "dir", "ignore")]);
  const summary = summarizePublish(context, draft, null, [
    change("a.txt", "added"),
    change("b/c.txt", "added"),
    change("cache/x.bin", "added"),
  ]);
  assert.equal(summary.isNew, true);
  assert.equal(summary.pushed, 2);
  assert.deepEqual(summary.added.sort(), ["a.txt", "b/c.txt"]);
  assert.deepEqual(summary.rulesAdded, [rule("cache", "dir", "ignore")]);
  assert.deepEqual(sizes(summary), { added: 2, rulesAdded: 1 });
});

test("pushing exactly the previous release says nothing", () => {
  const context = ctxOf(["a.txt", "mods/b.jar"]);
  const previous = release([before("a.txt"), before("mods/b.jar")]);
  const summary = summarizePublish(context, draftOf(), previous, []);
  assert.equal(summary.isNew, false);
  assert.equal(summary.pushed, 2);
  assert.equal(isSummaryEmpty(summary), true);
});

test("a modified file and an added one are pushed; a removed one is not", () => {
  const context = ctxOf(["a.txt", "new.txt"], [ghostOf("gone.txt")]);
  const previous = release([before("a.txt"), before("gone.txt")]);
  const summary = summarizePublish(context, draftOf(), previous, [
    change("a.txt", "modified"),
    change("new.txt", "added"),
  ]);
  assert.deepEqual(summary.modified, ["a.txt"]);
  assert.deepEqual(summary.added, ["new.txt"]);
  assert.deepEqual(summary.removed, ["gone.txt"]);
  assert.deepEqual(sizes(summary), { added: 1, modified: 1, removed: 1 });
});

test("setting a file of the previous release to ignore moves it out of the push", () => {
  const context = ctxOf(["a.txt", "b.txt"]);
  const previous = release([before("a.txt"), before("b.txt")]);
  const draft = setFileState(context, draftOf(), "a.txt", "ignore");
  const summary = summarizePublish(context, draft, previous, [change("a.txt", "modified")]);
  assert.equal(summary.pushed, 1);
  assert.deepEqual(summary.ignored, ["a.txt"]);
  // Not pushed, so its changed content is not part of the push.
  assert.deepEqual(summary.modified, []);
  assert.deepEqual(summary.rulesAdded, [rule("a.txt", "file", "ignore")]);
  assert.deepEqual(sizes(summary), { ignored: 1, rulesAdded: 1 });
});

test("setting a file to delete is told apart from ignore", () => {
  const context = ctxOf(["a.txt"], [ghostOf("g.txt")]);
  const previous = release([before("a.txt"), before("g.txt")]);
  let draft = setFileState(context, draftOf(), "a.txt", "delete");
  draft = setFileState(context, draft, "g.txt", "ignore");
  const summary = summarizePublish(context, draft, previous, []);
  assert.deepEqual(summary.deleted, ["a.txt"]);
  assert.deepEqual(summary.ignored, ["g.txt"]);
  assert.equal(summary.pushed, 0);
});

test("a new file set to ignore is not mentioned apart from its rule", () => {
  const context = ctxOf(["a.txt", "extra.txt"]);
  const previous = release([before("a.txt")]);
  const draft = setFileState(context, draftOf(), "extra.txt", "ignore");
  const summary = summarizePublish(context, draft, previous, [change("extra.txt", "added")]);
  assert.deepEqual(summary.added, []);
  assert.deepEqual(summary.rulesAdded, [rule("extra.txt", "file", "ignore")]);
});

test("dropping a rule of the previous release is a removed rule", () => {
  const context = ctxOf(["cache/x.bin"]);
  const rules = [rule("cache", "dir", "ignore"), rule("logs", "dir", "delete")];
  const previous = release([], rules);
  // `x.bin` was covered by the ignore rule; without it the file is pushed.
  const draft = draftOf([rules[1]]);
  const summary = summarizePublish(context, draft, previous, [change("cache/x.bin", "added")]);
  assert.deepEqual(summary.rulesRemoved, [rules[0]]);
  assert.deepEqual(summary.rulesAdded, []);
  assert.deepEqual(summary.added, ["cache/x.bin"]);
});

test("a rule that changes its state is a changed rule", () => {
  const context = ctxOf(["a.txt"]);
  const previous = release([], [rule("cache", "dir", "ignore")]);
  const draft = draftOf([rule("cache", "dir", "delete")]);
  const summary = summarizePublish(context, draft, previous, []);
  assert.deepEqual(summary.rulesChanged, [rule("cache", "dir", "delete")]);
  assert.deepEqual(summary.rulesAdded, []);
  assert.deepEqual(summary.rulesRemoved, []);
});

test("a file whose enabled state differs is toggled even when the content does too", () => {
  const context = ctxOf([
    { path: "mods/off.jar", disabled: true },
    { path: "mods/on.jar", disabled: false },
    { path: "mods/same.jar", disabled: false },
    { path: "mods/both.jar", disabled: true },
  ]);
  const previous = release([
    before("mods/off.jar"),
    before("mods/on.jar", { disabled: true }),
    before("mods/same.jar"),
    before("mods/both.jar"),
  ]);
  const summary = summarizePublish(context, draftOf(), previous, [
    change("mods/off.jar", "toggled"),
    change("mods/on.jar", "toggled"),
    change("mods/both.jar", "modified"),
  ]);
  assert.deepEqual(summary.enable.sort(), ["mods/on.jar"]);
  assert.deepEqual(summary.disable.sort(), ["mods/both.jar", "mods/off.jar"]);
  assert.deepEqual(summary.modified, ["mods/both.jar"]);
});

test("summary splits enabled and disabled state changes", () => {
  const context = ctxOf([
    { path: "mods/on.jar", disabled: false },
    { path: "mods/off.jar", disabled: true },
  ]);
  const previous = release([
    before("mods/on.jar", { disabled: true }),
    before("mods/off.jar", { disabled: false }),
  ]);
  const summary = summarizePublish(context, draftOf(), previous, []);
  assert.deepEqual(summary.enable, ["mods/on.jar"]);
  assert.deepEqual(summary.disable, ["mods/off.jar"]);
});

test("a locally deleted file keeps the summary visible when content is unchanged", () => {
  const context = ctxOf(["config/remove.cfg"]);
  const previous = release([before("config/remove.cfg")]);
  const draft = setFileState(context, draftOf(), "config/remove.cfg", "delete");
  const summary = summarizePublish(context, draft, previous, []);
  assert.deepEqual(summary.localDeleted, ["config/remove.cfg"]);
  assert.equal(isSummaryEmpty(summary), false);
});

test("local delete paths include real deleted files but not ghosts", () => {
  const context = ctxOf(["config/remove.cfg"], [ghostOf("config/ghost.cfg")]);
  const draft = setFileState(context, draftOf(), "config/remove.cfg", "delete");
  assert.deepEqual(localDeletePaths(context, draft), ["config/remove.cfg"]);
});

test("a directory delete counts inherited local files but an explicit ignore does not", () => {
  const context = ctxOf(["config/a.cfg", "config/keep.cfg"]);
  let draft = setDirState(context, draftOf(), "config", "delete");
  assert.deepEqual(localDeletePaths(context, draft).sort(), ["config/a.cfg", "config/keep.cfg"]);
  draft = setFileState(context, draft, "config/keep.cfg", "ignore");
  assert.deepEqual(localDeletePaths(context, draft), ["config/a.cfg"]);
});

test("first-install flags and content are told apart", () => {
  const context = ctxOf(["a.cfg", "b.cfg", "c.cfg", "d.cfg"]);
  const previous = release([
    before("a.cfg", { once: true }),
    before("b.cfg"),
    before("c.cfg", { once: true }),
    before("d.cfg", { once: true }),
  ]);
  // a stays first-install with new content, b becomes first-install, c turns
  // into an ordinary file with new content, d is unchanged.
  const draft = setOnce(
    setOnce(setOnce(draftOf([], { once: ["a.cfg", "d.cfg"] }), "b.cfg", true), "c.cfg", false),
    "d.cfg",
    true
  );
  const summary = summarizePublish(context, draft, previous, [
    change("a.cfg", "onceChanged"),
    change("c.cfg", "onceChanged"),
  ]);
  assert.deepEqual(summary.onceUpdated, ["a.cfg"]);
  assert.deepEqual(summary.onceOn, ["b.cfg"]);
  assert.deepEqual(summary.onceOff, ["c.cfg"]);
  assert.deepEqual(summary.modified, ["c.cfg"]);
});

test("a file that left the disk is a removal until it is ignored, and the summary follows the draft", () => {
  const context = ctxOf(["a.txt"], [ghostOf("old.txt")]);
  const previous = release([before("a.txt"), before("old.txt")]);
  const first = summarizePublish(context, draftOf(), previous, []);
  assert.deepEqual(first.removed, ["old.txt"]);
  const ignored = setFileState(context, draftOf(), "old.txt", "ignore");
  const second = summarizePublish(context, ignored, previous, []);
  assert.deepEqual(second.removed, []);
  assert.deepEqual(second.ignored, ["old.txt"]);
});

test("the summary always agrees with the payload the dialog sends", () => {
  const context = ctxOf(
    [
      "a.txt",
      "mods/b.jar",
      { path: "mods/c.jar", disabled: true },
      "cache/x.bin",
      "cache/keep.bin",
      "new.txt",
    ],
    [ghostOf("gone.txt"), ghostOf("lost", "dir")]
  );
  const previous = release(
    [before("a.txt"), before("mods/b.jar"), before("mods/c.jar"), before("gone.txt")],
    [rule("lost", "dir", "ignore")]
  );
  let draft = draftOf([rule("cache", "dir", "ignore")], { included: ["cache/keep.bin"] });
  draft = setFileState(context, draft, "mods/b.jar", "delete");
  draft = setOnce(draft, "new.txt", true);
  const changes = [
    change("a.txt", "modified"),
    change("mods/c.jar", "toggled"),
    change("new.txt", "added"),
    change("cache/x.bin", "added"),
    change("cache/keep.bin", "added"),
  ];
  const payload = toPublishPayload(context, draft);
  const summary = summarizePublish(context, draft, previous, changes);
  assert.equal(summary.pushed, payload.files.length);
  // Every pushed file is either new or a file of the previous release.
  const pushed = new Set(payload.files.map((file) => file.path));
  const listed = new Set(previous.files.map((file) => file.path));
  const added = new Set(summary.added);
  for (const file of pushed) assert.equal(added.has(file), !listed.has(file), file);
  for (const file of added) assert.ok(pushed.has(file), file);
  // Every file of the previous release that is not pushed is accounted for once.
  const gone = [...summary.removed, ...summary.ignored, ...summary.deleted].sort();
  assert.deepEqual(
    gone,
    previous.files.map((file) => file.path).filter((file) => !pushed.has(file)).sort()
  );
  assert.deepEqual(summary.rulesAdded, [
    rule("cache", "dir", "ignore"),
    rule("mods/b.jar", "file", "delete"),
  ]);
  assert.deepEqual(summary.rulesRemoved, [rule("lost", "dir", "ignore")]);
});
