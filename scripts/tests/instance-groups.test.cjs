const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function load(file, requireModule = () => ({})) {
  const module = { exports: {} };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(
      fs.readFileSync(path.join(__dirname, "../../", file), "utf8"),
      { compilerOptions: { module: ts.ModuleKind.CommonJS } }
    ).outputText
  )(module.exports, module, requireModule);
  return module.exports;
}
const installing = load("src/utils/installing-instance.ts");
const groupsLib = load("src/utils/instance-groups.ts", (name) =>
  name === "@/utils/installing-instance" ? installing : {}
);
const {
  addToNewGroup,
  buildSidebar,
  createGroup,
  deleteGroup,
  directorySectionKey,
  dropInstance,
  emptyInstanceGroups,
  isSortableSection,
  moveSection,
  normalizeGroups,
  removeInstanceFromGroups,
  renameGroup,
  toggleCollapsed,
  togglePinned,
} = groupsLib;

const inst = (id, gameDir, extra = {}) => ({
  id,
  name: id,
  folder: id,
  gameDir,
  starred: false,
  ...extra,
});
const instances = [
  inst("a", "Main", { starred: true }),
  inst("b", "Main"),
  inst("c", "Other"),
  inst("d", "Main", { campusInstallPending: true }),
];
const dirs = [
  { name: "Empty", title: "Empty" },
  { name: "Other", title: "Other" },
  { name: "Main", title: "Main" },
];
const ids = (section) => section.instances.map((item) => item.id);
const withCustom = () => {
  let g = createGroup(emptyInstanceGroups(), "g1", "One");
  g = createGroup(g, "g2", "Two");
  return g;
};
const auto = { key: "installed", kind: "installed" };
const starredRef = { key: "starred", kind: "starred" };
const custom = (key) => ({ key, kind: "custom" });

test("default layout lists installed then starred and hides empty installing", () => {
  const sections = buildSidebar(
    instances,
    emptyInstanceGroups(),
    "instance",
    dirs
  );
  assert.deepEqual(
    sections.map((s) => [s.key, s.kind]),
    [
      ["installing", "installing"],
      ["installed", "installed"],
      ["starred", "starred"],
    ]
  );
  assert.deepEqual(ids(sections[0]), ["d"]);
  assert.deepEqual(ids(sections[1]), ["a", "b", "c"]);
  assert.deepEqual(ids(sections[2]), ["a"]);
  const none = buildSidebar(
    instances.filter((i) => i.id !== "d"),
    undefined,
    "instance",
    dirs
  );
  assert.deepEqual(
    none.map((s) => s.key),
    ["installed", "starred"]
  );
});

test("order is pinned, installing, then the rest of order", () => {
  let g = withCustom();
  g = { ...g, order: ["g2", "starred", "installed", "g1"] };
  g = togglePinned(g, "g1");
  const keys = buildSidebar(instances, g, "instance", dirs).map((s) => s.key);
  assert.deepEqual(keys, ["g1", "installing", "g2", "starred", "installed"]);
});

test("normalize repairs missing, unknown and duplicate entries", () => {
  const g = normalizeGroups({
    order: ["gone", "g1", "g1"],
    pinned: "gone",
    custom: { g1: { name: "One", members: [] } },
  });
  assert.deepEqual(g.order, ["g1", "installed", "starred"]);
  assert.equal(g.pinned, null);
  assert.deepEqual(g.collapsed, []);
});

test("directory navigation expands installed in place, in settings order", () => {
  let g = withCustom();
  g = { ...g, order: ["g1", "installed", "starred", "g2"] };
  const sections = buildSidebar(instances, g, "directory", dirs);
  assert.deepEqual(
    sections.map((s) => s.key),
    [
      "installing",
      "g1",
      directorySectionKey("Other"),
      directorySectionKey("Main"),
      "starred",
      "g2",
    ]
  );
  const main = sections.find((s) => s.key === "installed:Main");
  assert.equal(main.kind, "installed");
  assert.deepEqual(ids(main), ["a", "b"]);
  assert.ok(!sections.some((s) => s.key === "installed:Empty"));
});

test("custom members show in join order and skip unknown ids", () => {
  let g = withCustom();
  g = dropInstance(g, "c", auto, custom("g1"), false).groups;
  g = dropInstance(g, "a", auto, custom("g1"), false).groups;
  g = {
    ...g,
    custom: {
      ...g.custom,
      g1: { ...g.custom.g1, members: [...g.custom.g1.members, "missing"] },
    },
  };
  const section = buildSidebar(instances, g, "instance", dirs).find(
    (s) => s.key === "g1"
  );
  assert.deepEqual(ids(section), ["c", "a"]);
  assert.deepEqual(g.custom.g1.members, ["c", "a", "missing"]);
});

test("moving between custom groups removes from the source", () => {
  let g = withCustom();
  g = dropInstance(g, "a", auto, custom("g1"), false).groups;
  const r = dropInstance(g, "a", custom("g1"), custom("g2"), false);
  assert.deepEqual(r.groups.custom.g1.members, []);
  assert.deepEqual(r.groups.custom.g2.members, ["a"]);
  assert.equal(r.starred, undefined);
});

test("copying (shift) only adds", () => {
  let g = withCustom();
  g = dropInstance(g, "a", auto, custom("g1"), false).groups;
  const r = dropInstance(g, "a", custom("g1"), custom("g2"), true);
  assert.deepEqual(r.groups.custom.g1.members, ["a"]);
  assert.deepEqual(r.groups.custom.g2.members, ["a"]);
});

test("automatic groups only copy out and accept nothing", () => {
  const g = withCustom();
  for (const source of [
    auto,
    { key: "installing", kind: "installing" },
    { key: "installed:Main", kind: "installed" },
  ]) {
    const r = dropInstance(g, "a", source, custom("g1"), false);
    assert.deepEqual(r.groups.custom.g1.members, ["a"]);
    assert.equal(r.starred, undefined);
    assert.equal(dropInstance(g, "a", custom("g1"), source, false), null);
    assert.equal(dropInstance(g, "a", custom("g1"), source, true), null);
  }
  assert.equal(dropInstance(g, "a", starredRef, auto, false), null);
});

test("dropping onto the same section does nothing", () => {
  const g = withCustom();
  assert.equal(dropInstance(g, "a", custom("g1"), custom("g1"), false), null);
  assert.equal(dropInstance(g, "a", starredRef, starredRef, false), null);
  assert.equal(dropInstance(g, "a", auto, custom("missing"), false), null);
});

test("dropping on starred stars, leaving starred unstars", () => {
  let g = withCustom();
  g = dropInstance(g, "b", auto, custom("g1"), false).groups;
  const star = dropInstance(g, "b", custom("g1"), starredRef, false);
  assert.equal(star.starred, true);
  assert.deepEqual(star.groups.custom.g1.members, []);
  const starCopy = dropInstance(g, "b", custom("g1"), starredRef, true);
  assert.equal(starCopy.starred, true);
  assert.deepEqual(starCopy.groups.custom.g1.members, ["b"]);
  const unstar = dropInstance(g, "a", starredRef, custom("g2"), false);
  assert.equal(unstar.starred, false);
  assert.deepEqual(unstar.groups.custom.g2.members, ["a"]);
  const unstarCopy = dropInstance(g, "a", starredRef, custom("g2"), true);
  assert.equal(unstarCopy.starred, undefined);
});

test("dropping on empty space removes from the source", () => {
  let g = withCustom();
  g = dropInstance(g, "a", auto, custom("g1"), false).groups;
  assert.deepEqual(
    dropInstance(g, "a", custom("g1"), null, false).groups.custom.g1.members,
    []
  );
  assert.equal(dropInstance(g, "a", custom("g1"), null, true), null);
  assert.equal(dropInstance(g, "a", auto, null, false), null);
  assert.equal(dropInstance(g, "a", custom("g2"), null, false), null);
  const r = dropInstance(g, "a", starredRef, null, false);
  assert.equal(r.starred, false);
});

test("a new group takes the instance and refuses empty or duplicate names", () => {
  const g = withCustom();
  const r = addToNewGroup(g, "g3", "  Three ", "a", custom("g1"), false);
  assert.equal(r.groups.custom.g3.name, "Three");
  assert.deepEqual(r.groups.custom.g3.members, ["a"]);
  assert.deepEqual(r.groups.order.slice(-1), ["g3"]);
  assert.equal(addToNewGroup(g, "g3", "one", "a", auto, false), null);
  assert.equal(addToNewGroup(g, "g3", "   ", "a", auto, false), null);
  const fromAuto = addToNewGroup(g, "g3", "Three", "a", auto, false);
  assert.deepEqual(fromAuto.groups.custom.g3.members, ["a"]);
});

test("pinning keeps one group and unpinning restores its slot", () => {
  let g = withCustom();
  g = togglePinned(g, "g2");
  assert.equal(g.pinned, "g2");
  g = togglePinned(g, "g1");
  assert.equal(g.pinned, "g1");
  g = togglePinned(g, "g1");
  assert.equal(g.pinned, null);
  assert.deepEqual(g.order, ["installed", "starred", "g1", "g2"]);
  assert.equal(togglePinned(g, "nope"), g);
});

test("rename rejects empty, duplicate and unchanged names", () => {
  const g = withCustom();
  assert.equal(renameGroup(g, "g1", "  "), g);
  assert.equal(renameGroup(g, "g1", "two"), g);
  assert.equal(renameGroup(g, "g1", "One"), g);
  assert.equal(renameGroup(g, "g1", " Uno ").custom.g1.name, "Uno");
});

test("deleting a group clears every reference but keeps the instances", () => {
  let g = withCustom();
  g = dropInstance(g, "a", auto, custom("g1"), false).groups;
  g = toggleCollapsed(togglePinned(g, "g1"), "g1");
  const next = deleteGroup(g, "g1");
  assert.deepEqual(Object.keys(next.custom), ["g2"]);
  assert.deepEqual(next.order, ["installed", "starred", "g2"]);
  assert.equal(next.pinned, null);
  assert.deepEqual(next.collapsed, []);
  assert.equal(deleteGroup(next, "g1"), next);
});

test("deleting an instance removes it from every group", () => {
  let g = withCustom();
  g = dropInstance(g, "a", auto, custom("g1"), false).groups;
  g = dropInstance(g, "a", auto, custom("g2"), false).groups;
  g = dropInstance(g, "b", auto, custom("g2"), false).groups;
  const next = removeInstanceFromGroups(g, "a");
  assert.deepEqual(next.custom.g1.members, []);
  assert.deepEqual(next.custom.g2.members, ["b"]);
  assert.equal(removeInstanceFromGroups(next, "zzz"), next);
});

test("collapsing toggles a key", () => {
  let g = toggleCollapsed(emptyInstanceGroups(), "starred");
  assert.deepEqual(g.collapsed, ["starred"]);
  g = toggleCollapsed(g, "starred");
  assert.deepEqual(g.collapsed, []);
});

test("sections reorder around a target, directories count as installed", () => {
  const g = withCustom();
  assert.deepEqual(moveSection(g, "g2", "installed", false).order, [
    "g2",
    "installed",
    "starred",
    "g1",
  ]);
  assert.deepEqual(moveSection(g, "installed", "g2", true).order, [
    "starred",
    "g1",
    "g2",
    "installed",
  ]);
  assert.deepEqual(moveSection(g, "installed:Main", "g1", true).order, [
    "starred",
    "g1",
    "installed",
    "g2",
  ]);
  assert.equal(moveSection(g, "installed", "installed:Main", true), g);
  assert.equal(moveSection(g, "installed", "starred", false), g);
});

test("the pinned group and installing cannot be sorted", () => {
  const g = togglePinned(withCustom(), "g1");
  assert.equal(isSortableSection(g, custom("g1")), false);
  assert.equal(isSortableSection(g, custom("g2")), true);
  assert.equal(
    isSortableSection(g, { key: "installing", kind: "installing" }),
    false
  );
  assert.equal(isSortableSection(g, starredRef), true);
  assert.equal(moveSection(g, "g1", "g2", true), g);
  assert.equal(moveSection(g, "g2", "g1", false), g);
});

test("every place that removes an instance also clears it from the groups", () => {
  const source = (file) =>
    fs.readFileSync(path.join(__dirname, "../../src", file), "utf8");
  for (const file of [
    "components/modals/delete-instance-alert-dialog.tsx",
    "hooks/discard-campus-install.ts",
  ]) {
    assert.match(source(file), /removeFromGroups\(/, file);
  }
});
