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
  MAX_RULES,
  buildPublishNodes,
  canRemoveFile,
  createInitialModel,
  nextState,
  normalizeDiskFiles,
  restoreInheritance,
  setDirState,
  setFileState,
  setOnce,
  toPublishPayload,
  viewOf,
} = load("src/utils/modpack-publish-tree.ts");

// Plan 《整合包发布三态与本地忽略》§3-§4 is the source of every expectation.

const entry = (p, extra = {}) => ({
  path: p,
  disabled: false,
  once: false,
  ...extra,
});
const rule = (p, kind, state) => ({ path: p, kind, state });
const ctxOf = (paths, ghosts = []) => ({
  files: normalizeDiskFiles(paths),
  ghosts,
});
const draftOf = (rules = [], { included = [], once = [] } = {}) => ({
  rules,
  included: new Set(included),
  once: new Set(once),
});
const model = (all, previous = null, suggested = []) =>
  createInitialModel({ all, previous, suggested });
const view = (m) => viewOf(m.context, m.draft);
const paths = (payload) => payload.files.map((f) => f.path).sort();

// ---------------------------------------------------------------- §4 defaults

test("first release: every file is ticked, no rules, nothing once or disabled", () => {
  const m = model(["config/a.toml", "mods/x.jar", "options.txt"]);
  const payload = toPublishPayload(m.context, m.draft);
  assert.deepEqual(paths(payload), [
    "config/a.toml",
    "mods/x.jar",
    "options.txt",
  ]);
  assert.deepEqual(payload.rules, []);
  assert.ok(payload.files.every((f) => !f.once && !f.disabled));
  assert.deepEqual(payload.conflicts, []);
  assert.deepEqual(m.context.ghosts, []);
});

test("first release: a suggested dir becomes ONE ignore rule at its own level", () => {
  const m = model(
    ["saves/w1/level.dat", "saves/w1/region/r.0.mca", "saves/w2/level.dat", "mods/x.jar"],
    null,
    [{ path: "saves", kind: "dir" }]
  );
  assert.deepEqual(m.draft.rules, [rule("saves", "dir", "ignore")]);
  const v = view(m);
  assert.equal(v.fileState("saves/w1/level.dat"), "ignore");
  assert.equal(v.fileState("saves/w1/region/r.0.mca"), "ignore");
  assert.equal(v.dirState("saves"), "ignore");
  const payload = toPublishPayload(m.context, m.draft);
  assert.deepEqual(paths(payload), ["mods/x.jar"]);
  assert.deepEqual(payload.rules, [rule("saves", "dir", "ignore")]);
});

test("first release: a suggested single file becomes a file rule, a nested dir a dir rule", () => {
  const m = model(
    ["options.txt", "mods/VoxelMods/a.json", "mods/VoxelMods/b.json", "mods/x.jar"],
    null,
    [
      { path: "options.txt", kind: "file" },
      { path: "mods/VoxelMods", kind: "dir" },
    ]
  );
  assert.equal(m.draft.rules.length, 2);
  assert.deepEqual(
    m.draft.rules.slice().sort((a, b) => a.path.localeCompare(b.path)),
    [rule("mods/VoxelMods", "dir", "ignore"), rule("options.txt", "file", "ignore")]
  );
  assert.deepEqual(paths(toPublishPayload(m.context, m.draft)), ["mods/x.jar"]);
});

test("previous release: files still on disk stay ticked and keep their once flag", () => {
  const m = model(["config/a.toml", "config/b.toml"], {
    files: [entry("config/a.toml", { once: true }), entry("config/b.toml")],
    rules: [],
  });
  const v = view(m);
  assert.equal(v.fileState("config/a.toml"), "include");
  assert.equal(v.isOnce("config/a.toml"), true);
  assert.equal(v.isOnce("config/b.toml"), false);
  const payload = toPublishPayload(m.context, m.draft);
  assert.deepEqual(
    payload.files.map((f) => [f.path, f.once]).sort(),
    [
      ["config/a.toml", true],
      ["config/b.toml", false],
    ]
  );
});

test("previous release: new files with no rule are ticked and not once", () => {
  const m = model(["config/a.toml", "config/new.toml"], {
    files: [entry("config/a.toml", { once: true })],
    rules: [],
  });
  const v = view(m);
  assert.equal(v.fileState("config/new.toml"), "include");
  assert.equal(v.isOnce("config/new.toml"), false);
});

test("previous release: its rules carry over as they were, even when the target is gone", () => {
  const prevRules = [
    rule("gone/dir", "dir", "delete"),
    rule("resourcepacks/old.zip", "file", "delete"),
    rule("config/cache", "dir", "ignore"),
  ];
  const m = model(
    ["resourcepacks/old.zip", "config/cache/x.bin", "config/a.toml"],
    { files: [entry("config/a.toml")], rules: prevRules }
  );
  assert.deepEqual(m.draft.rules, prevRules);
  assert.deepEqual(toPublishPayload(m.context, m.draft).rules, prevRules);
  const v = view(m);
  assert.equal(v.fileState("resourcepacks/old.zip"), "delete");
  assert.equal(v.fileState("config/cache/x.bin"), "ignore");
  assert.deepEqual(m.context.ghosts.find((g) => g.path === "gone/dir"), {
    path: "gone/dir",
    kind: "dir",
  });
});

test("previous release: a new file under a previous rule inherits it", () => {
  const m = model(["config/cache/new.bin"], {
    files: [],
    rules: [rule("config/cache", "dir", "delete")],
  });
  assert.equal(view(m).fileState("config/cache/new.bin"), "delete");
  assert.deepEqual(paths(toPublishPayload(m.context, m.draft)), []);
});

test("previous release: a file it listed under its own dir rule stays ticked, new siblings do not", () => {
  const m = model(["saves/keep.dat", "saves/other.dat"], {
    files: [entry("saves/keep.dat")],
    rules: [rule("saves", "dir", "ignore")],
  });
  const v = view(m);
  assert.equal(v.fileState("saves/keep.dat"), "include");
  assert.equal(v.fileState("saves/other.dat"), "ignore");
  assert.deepEqual(paths(toPublishPayload(m.context, m.draft)), ["saves/keep.dat"]);
  assert.deepEqual(m.draft.rules, [rule("saves", "dir", "ignore")]);
});

test("previous release without the suggested path: suggestion applies, previous rules kept", () => {
  const prevRules = [rule("config/cache", "dir", "ignore")];
  const m = model(
    ["config/a.toml", "saves/w1/level.dat"],
    { files: [entry("config/a.toml")], rules: prevRules },
    [{ path: "saves", kind: "dir" }]
  );
  assert.ok(m.draft.rules.some((r) => r.path === "config/cache" && r.state === "ignore"));
  assert.equal(m.draft.rules.filter((r) => r.path === "saves").length, 1);
  assert.deepEqual(m.draft.rules.find((r) => r.path === "saves"), rule("saves", "dir", "ignore"));
  assert.equal(m.draft.rules.length, 2);
  assert.equal(view(m).fileState("saves/w1/level.dat"), "ignore");
});

test("suggestion is skipped when the previous release listed a file inside the path", () => {
  const m = model(
    ["saves/w1/level.dat", "saves/w2/level.dat"],
    { files: [entry("saves/w1/level.dat")], rules: [] },
    [{ path: "saves", kind: "dir" }]
  );
  assert.deepEqual(m.draft.rules, []);
  const v = view(m);
  assert.equal(v.fileState("saves/w1/level.dat"), "include");
  assert.equal(v.fileState("saves/w2/level.dat"), "include");
});

test("suggestion is skipped when the previous release had a rule on or below the path", () => {
  const onPath = model(
    ["saves/w1/level.dat"],
    { files: [], rules: [rule("saves", "dir", "delete")] },
    [{ path: "saves", kind: "dir" }]
  );
  assert.deepEqual(onPath.draft.rules, [rule("saves", "dir", "delete")]);

  const below = model(
    ["saves/w1/level.dat"],
    { files: [], rules: [rule("saves/w1", "dir", "delete")] },
    [{ path: "saves", kind: "dir" }]
  );
  assert.deepEqual(below.draft.rules, [rule("saves/w1", "dir", "delete")]);

  const fileRule = model(
    ["options.txt"],
    { files: [], rules: [rule("options.txt", "file", "delete")] },
    [{ path: "options.txt", kind: "file" }]
  );
  assert.deepEqual(fileRule.draft.rules, [rule("options.txt", "file", "delete")]);
});

test("a previous path that merely shares a name prefix does not count as mentioning the suggestion", () => {
  const m = model(
    ["saves/w1/level.dat", "saves2/a.dat"],
    { files: [entry("saves2/a.dat")], rules: [] },
    [{ path: "saves", kind: "dir" }]
  );
  assert.deepEqual(m.draft.rules, [rule("saves", "dir", "ignore")]);
});

test("suggestion matching is case-insensitive", () => {
  const m = model(
    ["saves/w1/level.dat"],
    { files: [entry("Saves/W1/Level.dat")], rules: [] },
    [{ path: "saves", kind: "dir" }]
  );
  assert.deepEqual(m.draft.rules, []);
  assert.equal(view(m).fileState("saves/w1/level.dat"), "include");
});

// ---------------------------------------------------------------- virtual nodes

test("virtual file: listed by the previous release, missing on disk, shown as remove and never published", () => {
  const m = model(["a/keep.txt"], {
    files: [entry("a/keep.txt"), entry("a/old.txt", { once: true })],
    rules: [],
  });
  assert.deepEqual(m.context.ghosts, [{ path: "a/old.txt", kind: "file" }]);
  const v = view(m);
  assert.equal(v.fileState("a/old.txt"), "remove");
  const payload = toPublishPayload(m.context, m.draft);
  assert.deepEqual(paths(payload), ["a/keep.txt"]);
  assert.deepEqual(payload.rules, []);
});

test("virtual file cannot be ticked", () => {
  const m = model(["a/keep.txt"], {
    files: [entry("a/old.txt")],
    rules: [],
  });
  const next = setFileState(m.context, m.draft, "a/old.txt", "include");
  assert.equal(viewOf(m.context, next).fileState("a/old.txt"), "remove");
  assert.deepEqual(paths(toPublishPayload(m.context, next)), ["a/keep.txt"]);
  assert.deepEqual(next.rules, []);
  assert.equal(next.included.size, 0);
});

test("virtual file can be set to ignore or delete (file rule) and back to remove", () => {
  const m = model(["a/keep.txt"], { files: [entry("a/old.txt")], rules: [] });
  const ignored = setFileState(m.context, m.draft, "a/old.txt", "ignore");
  assert.deepEqual(ignored.rules, [rule("a/old.txt", "file", "ignore")]);
  assert.equal(viewOf(m.context, ignored).fileState("a/old.txt"), "ignore");

  const deleted = setFileState(m.context, ignored, "a/old.txt", "delete");
  assert.deepEqual(deleted.rules, [rule("a/old.txt", "file", "delete")]);

  const removed = setFileState(m.context, deleted, "a/old.txt", "remove");
  assert.deepEqual(removed.rules, []);
  assert.equal(viewOf(m.context, removed).fileState("a/old.txt"), "remove");

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
  const m = model(["config/a.toml"], {
    files: [],
    rules: [rule("gone.txt", "file", "ignore"), rule("gonedir", "dir", "delete")],
  });
  assert.deepEqual(
    m.context.ghosts.slice().sort((a, b) => a.path.localeCompare(b.path)),
    [
      { path: "gone.txt", kind: "file" },
      { path: "gonedir", kind: "dir" },
    ]
  );
  const nodes = buildPublishNodes(m.context);
  const gonedir = nodes.find((n) => n.path === "gonedir");
  assert.ok(gonedir);
  assert.equal(gonedir.ghost, true);
  assert.equal(gonedir.kind, "dir");
});

test("buildPublishNodes: ghost flag only on missing files, real parents are not ghosts", () => {
  const m = model(["a/keep.txt"], { files: [entry("a/old.txt")], rules: [] });
  const [dir] = buildPublishNodes(m.context);
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
  const m = model(["a/x.txt", "a/y.txt"]);
  const ignored = setFileState(m.context, m.draft, "a/x.txt", "ignore");
  assert.deepEqual(ignored.rules, [rule("a/x.txt", "file", "ignore")]);
  const deleted = setFileState(m.context, ignored, "a/x.txt", "delete");
  assert.deepEqual(deleted.rules, [rule("a/x.txt", "file", "delete")]);
  const back = setFileState(m.context, deleted, "a/x.txt", "include");
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
  const m = model(["a/x.txt", "a/b/y.txt", "c/z.txt"]);
  const next = setDirState(m.context, m.draft, "a", "ignore");
  assert.deepEqual(next.rules, [rule("a", "dir", "ignore")]);
  const v = viewOf(m.context, next);
  assert.equal(v.fileState("a/b/y.txt"), "ignore");
  assert.equal(v.fileState("c/z.txt"), "include");
  const del = setDirState(m.context, next, "a", "delete");
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

test("mods/*.jar.disabled is listed under its enabled name with disabled: true", () => {
  const files = normalizeDiskFiles(["mods/a.jar.disabled", "mods/b.jar", "mods/sub/c.jar.disabled"]);
  const byPath = Object.fromEntries(files.map((f) => [f.path, f]));
  assert.equal(byPath["mods/a.jar"].disabled, true);
  assert.equal(byPath["mods/b.jar"].disabled, false);
  assert.equal(byPath["mods/sub/c.jar"].disabled, true);
  assert.equal(files.length, 3);
  assert.ok(files.every((f) => !f.path.endsWith(".disabled") && !f.conflict));
});

test("a disabled mod is published under the enabled name with disabled: true", () => {
  const m = model(["mods/a.jar.disabled", "mods/b.jar"]);
  const payload = toPublishPayload(m.context, m.draft);
  assert.deepEqual(
    payload.files.map((f) => [f.path, f.disabled, f.once]).sort(),
    [
      ["mods/a.jar", true, false],
      ["mods/b.jar", false, false],
    ]
  );
  assert.ok(payload.files.every((f) => !f.path.endsWith(".disabled")));
});

test("the .disabled suffix is recognised case-insensitively", () => {
  const files = normalizeDiskFiles(["Mods/A.JAR.DISABLED"]);
  assert.equal(files.length, 1);
  assert.equal(files[0].path, "Mods/A.JAR");
  assert.equal(files[0].disabled, true);
});

test("previous release listing the enabled name matches a disabled file on disk (not a virtual node)", () => {
  const m = model(["mods/a.jar.disabled"], {
    files: [entry("mods/a.jar", { once: true })],
    rules: [],
  });
  assert.deepEqual(m.context.ghosts, []);
  const payload = toPublishPayload(m.context, m.draft);
  assert.deepEqual(payload.files, [{ path: "mods/a.jar", disabled: true, once: true }]);
});

test("the disk decides disabled: a mod the previous release shipped enabled is published disabled once disabled locally", () => {
  const m = model(["mods/a.jar.disabled"], {
    files: [entry("mods/a.jar", { disabled: false })],
    rules: [],
  });
  assert.equal(toPublishPayload(m.context, m.draft).files[0].disabled, true);
});

test("both variants on disk are flagged as a conflict", () => {
  const files = normalizeDiskFiles(["mods/a.jar", "mods/a.jar.disabled", "mods/b.jar"]);
  const a = files.filter((f) => f.path === "mods/a.jar");
  assert.equal(a.length, 1);
  assert.equal(a[0].conflict, true);
  assert.equal(files.find((f) => f.path === "mods/b.jar").conflict, false);
});

test("both variants on disk: the conflict is reported for the ticked file only", () => {
  const all = ["mods/a.jar", "mods/a.jar.disabled", "mods/b.jar"];
  const m = model(all);
  assert.deepEqual(toPublishPayload(m.context, m.draft).conflicts, ["mods/a.jar"]);
  const ignored = {
    ...m.draft,
    rules: [rule("mods/a.jar", "file", "ignore")],
  };
  assert.deepEqual(toPublishPayload(m.context, ignored).conflicts, []);
});

test("the conflict is detected whatever the listing order or case of the variants", () => {
  const a = normalizeDiskFiles(["mods/a.jar.disabled", "mods/a.jar"]);
  assert.equal(a.length, 1);
  assert.equal(a[0].conflict, true);
  const b = normalizeDiskFiles(["Mods/A.jar.disabled", "mods/a.JAR"]);
  assert.equal(b.length, 1);
  assert.equal(b[0].conflict, true);
});

test("a .disabled file outside mods/ is never published as disabled", () => {
  const all = [
    "config/a.toml.disabled",
    "modsx/a.jar.disabled",
    "resourcepacks/p.zip.disabled",
    "x.disabled",
    "mods/ok.jar",
  ];
  const files = normalizeDiskFiles(all);
  assert.ok(files.every((f) => !(f.disabled && !f.path.toLowerCase().startsWith("mods/"))));
  const m = model(all);
  const payload = toPublishPayload(m.context, m.draft);
  assert.ok(payload.files.every((f) => !f.path.toLowerCase().endsWith(".disabled")));
  assert.ok(payload.files.every((f) => !f.disabled));
  assert.deepEqual(paths(payload), ["mods/ok.jar"]);
});

test("buildPublishNodes marks a disabled mod under its enabled name", () => {
  const m = model(["mods/a.jar.disabled", "mods/b.jar", "mods/c.jar", "mods/c.jar.disabled"]);
  const mods = buildPublishNodes(m.context).find((n) => n.path === "mods");
  const byName = Object.fromEntries(mods.children.map((c) => [c.name, c]));
  assert.equal(byName["a.jar"].disabled, true);
  assert.equal(byName["b.jar"].disabled, false);
  assert.equal(byName["c.jar"].conflict, true);
  assert.equal(byName["a.jar.disabled"], undefined);
});

test("buildPublishNodes lists directories before files", () => {
  const m = model(["z.txt", "a/x.txt", "b.txt"]);
  const nodes = buildPublishNodes(m.context);
  assert.deepEqual(nodes.map((n) => n.name), ["a", "b.txt", "z.txt"]);
});

// ---------------------------------------------------------------- casefold

test("the payload never holds two files whose paths differ only by case", () => {
  const m = model(["a/X.txt", "a/x.txt", "b/y.txt"]);
  const keys = toPublishPayload(m.context, m.draft).files.map((f) => f.path.toLowerCase());
  assert.equal(new Set(keys).size, keys.length);
  assert.ok(keys.includes("b/y.txt"));
});

test("previous-release once and ticks are matched case-insensitively", () => {
  const m = model(["config/a.toml"], {
    files: [entry("Config/A.toml", { once: true })],
    rules: [],
  });
  assert.deepEqual(m.context.ghosts, []);
  assert.equal(view(m).isOnce("config/a.toml"), true);
});

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
  const m = model([], {
    files: [entry("cache/old.bin")],
    rules: [rule("cache", "dir", "ignore")],
  });
  assert.equal(canRemoveFile(m.draft, "cache/old.bin"), false);
  let draft = m.draft;
  for (let i = 0; i < 5; i++) {
    const current = viewOf(m.context, draft).fileState("cache/old.bin");
    draft = setFileState(m.context, draft, "cache/old.bin", nextState(current, ghost));
    const after = viewOf(m.context, draft).fileState("cache/old.bin");
    assert.ok(after === "ignore" || after === "delete");
  }
  assert.deepEqual(toPublishPayload(m.context, draft).files, []);
});

test("a virtual directory cycles remove, ignore, delete and is not ticked", () => {
  const m = model(["a.txt"], { files: [], rules: [rule("gone", "dir", "delete")] });
  assert.deepEqual(m.context.ghosts, [{ path: "gone", kind: "dir" }]);
  assert.equal(viewOf(m.context, m.draft).dirState("gone"), "delete");
  const restored = restoreInheritance(m.draft, "gone", "dir");
  assert.deepEqual(restored.rules, []);
  assert.equal(viewOf(m.context, restored).dirState("gone"), "remove");
  const nodes = buildPublishNodes(m.context);
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

test("a dir rule that matches an existing directory by case is not a virtual node", () => {
  const m = model(["Config/Cache/a.bin"], {
    files: [],
    rules: [rule("config/cache", "dir", "ignore")],
  });
  assert.deepEqual(m.context.ghosts, []);
  assert.equal(view(m).fileState("Config/Cache/a.bin"), "ignore");
});
