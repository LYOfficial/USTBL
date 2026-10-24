const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

// Transpile the TS module on the fly; it has no imports.
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
          target: ts.ScriptTarget.ES2020,
        },
      }
    ).outputText
  )(exports, module, () => ({}));
  return module.exports;
}

const {
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

const model = (all, previous = null, suggested = []) =>
  createInitialModel({ all, previous, suggested });
const paths = (payload) => payload.files.map((file) => file.path);
const rule = (path, kind, state) => ({ path, kind, state });
const prevFile = (path, extra = {}) => ({
  path,
  disabled: false,
  once: false,
  ...extra,
});

test("a disabled mod is shown under its enabled name and ships disabled", () => {
  const files = normalizeDiskFiles([
    "mods/a.jar",
    "mods/b.jar.disabled",
    "config/c.toml.disabled",
  ]);
  assert.deepEqual(files, [
    { path: "mods/a.jar", disabled: false, conflict: false },
    { path: "mods/b.jar", disabled: true, conflict: false },
  ]);
  const { context, draft } = model(["mods/b.jar.disabled"]);
  assert.deepEqual(toPublishPayload(context, draft).files, [
    { path: "mods/b.jar", disabled: true, once: false },
  ]);
});

test("both variants on disk are flagged, and reported only when ticked", () => {
  const { context, draft } = model(["mods/a.jar", "mods/a.jar.disabled"]);
  assert.equal(context.files.length, 1);
  assert.equal(context.files[0].conflict, true);
  assert.deepEqual(toPublishPayload(context, draft).conflicts, ["mods/a.jar"]);
  const ignored = setFileState(context, draft, "mods/a.jar", "ignore");
  assert.deepEqual(toPublishPayload(context, ignored).conflicts, []);
});

test("first publish ticks every new file and writes no rules", () => {
  const { context, draft } = model(["mods/a.jar", "config/x.toml"]);
  const payload = toPublishPayload(context, draft);
  assert.deepEqual(paths(payload).sort(), ["config/x.toml", "mods/a.jar"]);
  assert.deepEqual(payload.rules, []);
});

test("a suggested entry becomes one rule at its own level, not one per file", () => {
  const { context, draft } = model(
    ["saves/w1/level.dat", "saves/w2/level.dat", "options.txt", "mods/a.jar"],
    null,
    [
      { path: "saves", kind: "dir" },
      { path: "options.txt", kind: "file" },
    ]
  );
  const payload = toPublishPayload(context, draft);
  assert.deepEqual(payload.rules, [
    rule("saves", "dir", "ignore"),
    rule("options.txt", "file", "ignore"),
  ]);
  assert.deepEqual(paths(payload), ["mods/a.jar"]);
});

test("a suggested entry the previous release mentioned is left alone", () => {
  const suggested = [{ path: "saves", kind: "dir" }];
  const all = ["saves/w1/level.dat", "mods/a.jar"];
  const viaFile = model(
    all,
    { files: [prevFile("saves/w1/level.dat")], rules: [] },
    suggested
  );
  assert.deepEqual(viaFile.draft.rules, []);
  assert.deepEqual(
    paths(toPublishPayload(viaFile.context, viaFile.draft)).sort(),
    ["mods/a.jar", "saves/w1/level.dat"]
  );
  const viaRule = model(
    all,
    { files: [], rules: [rule("saves", "dir", "delete")] },
    suggested
  );
  assert.deepEqual(viaRule.draft.rules, [rule("saves", "dir", "delete")]);
  const viaAncestor = model(
    ["mods/VoxelMods/x.json"],
    { files: [], rules: [rule("mods", "dir", "ignore")] },
    [{ path: "mods/VoxelMods", kind: "dir" }]
  );
  assert.deepEqual(viaAncestor.draft.rules, [rule("mods", "dir", "ignore")]);
});

test("previous files stay ticked with their once flag", () => {
  const { context, draft } = model(["config/x.toml", "mods/a.jar"], {
    files: [prevFile("config/x.toml", { once: true }), prevFile("mods/a.jar")],
    rules: [],
  });
  assert.deepEqual(toPublishPayload(context, draft).files, [
    { path: "config/x.toml", disabled: false, once: true },
    { path: "mods/a.jar", disabled: false, once: false },
  ]);
});

test("previous rules carry over as they were, even for targets that are gone", () => {
  const rules = [
    rule("resourcepacks/old.zip", "file", "delete"),
    rule("config/cache", "dir", "ignore"),
    rule("gone", "dir", "delete"),
  ];
  const { context, draft } = model(["config/cache/a.bin", "mods/a.jar"], {
    files: [],
    rules,
  });
  assert.deepEqual(draft.rules, rules);
  assert.deepEqual(toPublishPayload(context, draft).rules, rules);
  assert.deepEqual(
    context.ghosts.map((g) => g.path),
    ["resourcepacks/old.zip", "gone"]
  );
});

test("a new file follows the nearest rule, a previous file under a rule stays ticked", () => {
  const { context, draft } = model(
    ["cache/new.bin", "cache/keep.bin", "cache/deep/x.bin", "other/y.bin"],
    {
      files: [prevFile("cache/keep.bin")],
      rules: [
        rule("cache", "dir", "ignore"),
        rule("cache/deep", "dir", "delete"),
      ],
    }
  );
  const view = viewOf(context, draft);
  assert.equal(view.fileState("cache/new.bin"), "ignore");
  assert.equal(view.fileState("cache/keep.bin"), "include");
  assert.equal(view.fileState("cache/deep/x.bin"), "delete");
  assert.equal(view.fileState("other/y.bin"), "include");
  assert.deepEqual(paths(toPublishPayload(context, draft)).sort(), [
    "cache/keep.bin",
    "other/y.bin",
  ]);
});

test("rule priority: ticked file, then file rule, then the deepest dir rule", () => {
  const { context, draft } = model(["a/b/c.txt", "a/b/d.txt", "a/e.txt"], {
    files: [prevFile("a/b/c.txt")],
    rules: [
      rule("a", "dir", "ignore"),
      rule("a/b", "dir", "delete"),
      rule("a/b/d.txt", "file", "ignore"),
    ],
  });
  const view = viewOf(context, draft);
  assert.equal(view.fileState("a/b/c.txt"), "include");
  assert.equal(view.fileState("a/b/d.txt"), "ignore");
  assert.equal(view.fileState("a/e.txt"), "ignore");
  const other = viewOf(
    context,
    setFileState(context, draft, "a/b/c.txt", "delete")
  );
  assert.equal(other.fileState("a/b/c.txt"), "delete");
});

test("rule matching ignores case", () => {
  const { context, draft } = model(["Config/Cache/a.bin"], {
    files: [],
    rules: [rule("config/cache", "dir", "ignore")],
  });
  assert.equal(
    viewOf(context, draft).fileState("Config/Cache/a.bin"),
    "ignore"
  );
  assert.equal(context.ghosts.length, 0);
});

test("a missing previous file is a ghost: removed by default, never ticked", () => {
  const { context, draft } = model(["mods/a.jar"], {
    files: [prevFile("mods/a.jar"), prevFile("mods/old.jar")],
    rules: [],
  });
  assert.deepEqual(context.ghosts, [{ path: "mods/old.jar", kind: "file" }]);
  const view = viewOf(context, draft);
  assert.equal(view.fileState("mods/old.jar"), "remove");
  assert.deepEqual(paths(toPublishPayload(context, draft)), ["mods/a.jar"]);

  const ticked = setFileState(context, draft, "mods/old.jar", "include");
  assert.deepEqual(ticked, draft);
  assert.deepEqual(paths(toPublishPayload(context, ticked)), ["mods/a.jar"]);

  const ignored = setFileState(context, draft, "mods/old.jar", "ignore");
  assert.deepEqual(ignored.rules, [rule("mods/old.jar", "file", "ignore")]);
  assert.deepEqual(paths(toPublishPayload(context, ignored)), ["mods/a.jar"]);
  const back = setFileState(context, ignored, "mods/old.jar", "remove");
  assert.deepEqual(back.rules, []);
  assert.equal(canRemoveFile(draft, "mods/old.jar"), true);
});

test("a ghost under a covering rule cannot go back to removed", () => {
  const { context, draft } = model([], {
    files: [prevFile("cache/old.bin")],
    rules: [rule("cache", "dir", "ignore")],
  });
  assert.equal(viewOf(context, draft).fileState("cache/old.bin"), "ignore");
  assert.equal(canRemoveFile(draft, "cache/old.bin"), false);
});

test("a file rule appears only where the state differs from what is inherited", () => {
  const { context, draft } = model(["a/x.txt", "b/y.txt"], {
    files: [],
    rules: [rule("a", "dir", "ignore")],
  });
  const same = setFileState(context, draft, "a/x.txt", "ignore");
  assert.deepEqual(same.rules, [rule("a", "dir", "ignore")]);
  const differs = setFileState(context, draft, "a/x.txt", "delete");
  assert.deepEqual(differs.rules, [
    rule("a", "dir", "ignore"),
    rule("a/x.txt", "file", "delete"),
  ]);
  const ticked = setFileState(context, differs, "a/x.txt", "include");
  assert.deepEqual(ticked.rules, [rule("a", "dir", "ignore")]);
  assert.deepEqual(paths(toPublishPayload(context, ticked)).sort(), [
    "a/x.txt",
    "b/y.txt",
  ]);
  const plain = setFileState(context, draft, "b/y.txt", "ignore");
  assert.deepEqual(plain.rules, [
    rule("a", "dir", "ignore"),
    rule("b/y.txt", "file", "ignore"),
  ]);
});

test("a ticked file never carries a file rule", () => {
  const { context, draft } = model(["x.txt"]);
  const ignored = setFileState(context, draft, "x.txt", "ignore");
  const ticked = setFileState(context, ignored, "x.txt", "include");
  assert.deepEqual(ticked.rules, []);
  const payload = toPublishPayload(context, ticked);
  for (const file of payload.files) {
    assert.ok(
      !payload.rules.some((r) => r.kind === "file" && r.path === file.path)
    );
  }
});

test("rules are saved as set, never compressed into a directory rule", () => {
  let current = model(["d/a", "d/b", "d/c"]);
  let { context, draft } = current;
  for (const file of ["d/a", "d/b", "d/c"]) {
    draft = setFileState(context, draft, file, "ignore");
  }
  assert.deepEqual(toPublishPayload(context, draft).rules, [
    rule("d/a", "file", "ignore"),
    rule("d/b", "file", "ignore"),
    rule("d/c", "file", "ignore"),
  ]);
  assert.equal(viewOf(context, draft).dirState("d"), "ignore");
});

test("setting a directory leaves one rule and drops the settings below it", () => {
  const { context, draft } = model(["d/a", "d/e/b", "o/c"], {
    files: [],
    rules: [rule("d/e/b", "file", "delete"), rule("d/e", "dir", "delete")],
  });
  const ignored = setDirState(context, draft, "d", "ignore");
  assert.deepEqual(ignored.rules, [rule("d", "dir", "ignore")]);
  const view = viewOf(context, ignored);
  assert.equal(view.fileState("d/e/b"), "ignore");
  assert.equal(view.dirState("d"), "ignore");
  assert.equal(view.dirState("o"), "include");
  assert.deepEqual(paths(toPublishPayload(context, ignored)), ["o/c"]);

  const again = setDirState(context, ignored, "d", "delete");
  assert.deepEqual(again.rules, [rule("d", "dir", "delete")]);
});

test("ticking a directory under a rule puts its current files into files", () => {
  const { context, draft } = model(["cache/sub/a", "cache/sub/b", "cache/c"], {
    files: [],
    rules: [rule("cache", "dir", "ignore")],
  });
  const ticked = setDirState(context, draft, "cache/sub", "include");
  assert.deepEqual(ticked.rules, [rule("cache", "dir", "ignore")]);
  assert.deepEqual(paths(toPublishPayload(context, ticked)).sort(), [
    "cache/sub/a",
    "cache/sub/b",
  ]);
  const view = viewOf(context, ticked);
  assert.equal(view.dirState("cache/sub"), "include");
  assert.equal(view.dirState("cache"), "mixed");
  // A file added later still follows the outer rule.
  const grown = createInitialModel({
    all: ["cache/sub/a", "cache/sub/b", "cache/c", "cache/sub/new"],
    previous: {
      files: [prevFile("cache/sub/a"), prevFile("cache/sub/b")],
      rules: ticked.rules,
    },
    suggested: [],
  });
  assert.equal(
    viewOf(grown.context, grown.draft).fileState("cache/sub/new"),
    "ignore"
  );
});

test("ticking a directory with no covering rule needs no entries", () => {
  const { context, draft } = model(["d/a", "d/b"]);
  const ignored = setDirState(context, draft, "d", "ignore");
  const ticked = setDirState(context, ignored, "d", "include");
  assert.deepEqual(ticked.rules, []);
  assert.equal(ticked.included.size, 0);
  assert.deepEqual(paths(toPublishPayload(context, ticked)), ["d/a", "d/b"]);
});

test("restoring inheritance on a directory drops only its own rule", () => {
  const { context, draft } = model(["d/a", "d/e/b"], {
    files: [],
    rules: [rule("d", "dir", "ignore"), rule("d/e", "dir", "delete")],
  });
  const restored = restoreInheritance(draft, "d", "dir");
  assert.deepEqual(restored.rules, [rule("d/e", "dir", "delete")]);
  const view = viewOf(context, restored);
  assert.equal(view.fileState("d/a"), "include");
  assert.equal(view.fileState("d/e/b"), "delete");
  assert.equal(view.hasOverride("d", "dir"), false);
  assert.equal(view.hasOverride("d/e", "dir"), true);
});

test("restoring inheritance on a file goes back to the covering rule", () => {
  const { context, draft } = model(["d/a"], {
    files: [prevFile("d/a")],
    rules: [rule("d", "dir", "ignore")],
  });
  const view = viewOf(context, draft);
  assert.equal(view.fileState("d/a"), "include");
  assert.equal(view.hasOverride("d/a", "file"), true);
  const restored = restoreInheritance(draft, "d/a", "file");
  assert.equal(viewOf(context, restored).fileState("d/a"), "ignore");
});

test("restoring a rule on a ghost directory leaves it as removed", () => {
  const { context, draft } = model(["a.txt"], {
    files: [],
    rules: [rule("gone", "dir", "delete")],
  });
  assert.equal(viewOf(context, draft).dirState("gone"), "delete");
  const restored = restoreInheritance(draft, "gone", "dir");
  assert.deepEqual(restored.rules, []);
  assert.equal(viewOf(context, restored).dirState("gone"), "remove");
});

test("first-install-only is a per-file flag that ships only with ticked files", () => {
  const { context, draft } = model(["config/x.toml", "config/y.toml"]);
  const once = setOnce(draft, "config/x.toml", true);
  assert.equal(viewOf(context, once).isOnce("config/x.toml"), true);
  assert.deepEqual(toPublishPayload(context, once).files, [
    { path: "config/x.toml", disabled: false, once: true },
    { path: "config/y.toml", disabled: false, once: false },
  ]);
  const ignored = setFileState(context, once, "config/x.toml", "ignore");
  assert.deepEqual(paths(toPublishPayload(context, ignored)), [
    "config/y.toml",
  ]);
  const off = setOnce(once, "config/x.toml", false);
  assert.equal(viewOf(context, off).isOnce("config/x.toml"), false);
});

test("an empty selection with rules is a valid payload", () => {
  const { context, draft } = model(["a.txt"]);
  const ignored = setFileState(context, draft, "a.txt", "delete");
  const payload = toPublishPayload(context, ignored);
  assert.deepEqual(payload.files, []);
  assert.deepEqual(payload.rules, [rule("a.txt", "file", "delete")]);
});

test("a click cycles include, ignore, delete; a ghost cycles remove first", () => {
  assert.equal(nextState("include", { canRemove: false }), "ignore");
  assert.equal(nextState("ignore", { canRemove: false }), "delete");
  assert.equal(nextState("delete", { canRemove: false }), "include");
  assert.equal(nextState("mixed", { canRemove: false }), "include");
  assert.equal(nextState("remove", { canRemove: true }), "ignore");
  assert.equal(nextState("delete", { canRemove: true }), "remove");
});

test("the tree lists directories first and marks nodes missing from disk", () => {
  const { context } = model(["b.txt", "mods/a.jar", "a/z.txt"], {
    files: [prevFile("mods/old.jar")],
    rules: [rule("gone", "dir", "ignore")],
  });
  const nodes = buildPublishNodes(context);
  assert.deepEqual(
    nodes.map((node) => `${node.kind}:${node.name}:${node.ghost}`),
    ["dir:a:false", "dir:gone:true", "dir:mods:false", "file:b.txt:false"]
  );
  const mods = nodes.find((node) => node.name === "mods");
  assert.deepEqual(
    mods.children.map((node) => `${node.name}:${node.ghost}`),
    ["a.jar:false", "old.jar:true"]
  );
});
