const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function read(file) {
  return fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
}

function load(file) {
  const module = { exports: {} };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(read(file), {
      compilerOptions: { module: ts.ModuleKind.CommonJS },
    }).outputText
  )(module.exports, module, () => ({}));
  return module.exports;
}

const {
  AUTO_EXPAND_LIMIT,
  countFiles,
  createPathMatcher,
  filterTreeByQuery,
  isQueryActive,
  matchesQuery,
} = load("src/utils/fuzzy-filter.ts");

const file = (p) => ({ path: p, kind: "file", children: [] });
const dir = (p, children) => ({ path: p, kind: "dir", children });

test("matching ignores case", () => {
  assert.ok(matchesQuery("config/JEI/Client.cfg", "jei"));
  assert.ok(matchesQuery("config/jei/client.cfg", "CLIENT"));
});

test("a term is a subsequence of the path", () => {
  assert.ok(matchesQuery("mods/optifine_HD_U.jar", "ofhd"));
  assert.ok(!matchesQuery("mods/optifine_HD_U.jar", "hdof"));
  assert.ok(!matchesQuery("mods/a.jar", "zz"));
});

test("every term must match, in any order", () => {
  assert.ok(matchesQuery("config/jei/client.cfg", "client jei"));
  assert.ok(matchesQuery("config/jei/client.cfg", "  jei   cfg "));
  assert.ok(!matchesQuery("config/jei/client.cfg", "jei server"));
});

test("an empty query keeps everything", () => {
  assert.equal(isQueryActive(""), false);
  assert.equal(isQueryActive("   "), false);
  assert.ok(matchesQuery("anything", ""));
  assert.ok(createPathMatcher("  ")("x"));
  assert.equal(isQueryActive("a"), true);
});

test("both kinds of slash are the same", () => {
  assert.ok(matchesQuery("config\\jei\\client.cfg", "config/jei"));
  assert.ok(matchesQuery("config/jei/client.cfg", "config\\jei"));
  assert.ok(matchesQuery("config/jei/client.cfg", "g/j"));
});

test("the tree keeps matching files and the directories above them", () => {
  const tree = [
    dir("config", [
      dir("config/jei", [file("config/jei/a.cfg")]),
      file("config/b.cfg"),
    ]),
    dir("mods", [file("mods/x.jar")]),
    file("options.txt"),
  ];
  const result = filterTreeByQuery(tree, "jei");
  assert.equal(result.length, 1);
  assert.equal(result[0].path, "config");
  assert.deepEqual(
    result[0].children.map((node) => node.path),
    ["config/jei"]
  );
  assert.equal(result[0].children[0].children[0].path, "config/jei/a.cfg");
  assert.equal(countFiles(result), 1);
  // The input is left alone.
  assert.equal(tree[0].children.length, 2);
});

test("a query that hits a directory name keeps the files under it", () => {
  const tree = [dir("mods", [file("mods/x.jar"), file("mods/y.jar")])];
  assert.equal(countFiles(filterTreeByQuery(tree, "mods")), 2);
});

test("a directory without matching files drops out, an empty query returns the tree", () => {
  const tree = [dir("mods", [file("mods/x.jar")]), dir("empty", [])];
  assert.deepEqual(filterTreeByQuery(tree, "zzz"), []);
  assert.equal(filterTreeByQuery(tree, ""), tree);
  assert.equal(filterTreeByQuery(tree, "x").length, 1);
});

test("auto expansion has a cap", () => {
  assert.ok(AUTO_EXPAND_LIMIT > 0);
});

test("the publish tree, update preview and local changes all use the shared filter", () => {
  for (const file of [
    "src/components/common/publish-file-tree.tsx",
    "src/components/modals/update-modpack-modal.tsx",
    "src/components/modals/modpack-local-changes-modal.tsx",
  ]) {
    const source = read(file);
    assert.match(source, /@\/components\/common\/file-filter-input/, file);
    assert.match(source, /<FileFilterInput/, file);
    assert.match(source, /@\/utils\/fuzzy-filter/, file);
  }
  assert.match(
    read("src/components/common/publish-file-tree.tsx"),
    /filterTreeByQuery\(markedNodes, query\)/
  );
});

test("update preview filters the display, not the answers", () => {
  const source = read("src/components/modals/update-modpack-modal.tsx");
  const chooseAll = source.slice(
    source.indexOf("const chooseAll"),
    source.indexOf("// Display only")
  );
  assert.match(chooseAll, /plan\.conflicts/);
  assert.doesNotMatch(chooseAll, /shownConflicts|fileQuery/);
});

test("the input debounces typing", () => {
  const source = read("src/components/common/file-filter-input.tsx");
  assert.match(source, /FILTER_DEBOUNCE_MS = 150/);
  assert.match(source, /setTimeout/);
});
