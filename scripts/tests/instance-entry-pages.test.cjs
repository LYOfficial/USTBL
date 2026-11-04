const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

function read(file) {
  return fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
}

test("add-import sends campus and public modpacks to the discover pages", () => {
  const source = read("src/pages/instances/add-import.tsx");
  assert.match(
    source,
    /campusModpack: \(\) => router\.push\("\/discover\/campus-modpacks"\)/
  );
  assert.match(
    source,
    /discoverModpacks: \(\) => router\.push\("\/discover\/modpacks"\)/
  );
  assert.ok(
    source.indexOf("campusModpack:") < source.indexOf("discoverModpacks:")
  );
  assert.doesNotMatch(source, /instances\/sync/);
});

test("the install campus modpack button on the instance list opens the discover page", () => {
  const source = read("src/pages/instances/list/[[...dir]].tsx");
  assert.doesNotMatch(source, /instances\/sync/);
  assert.match(source, /router\.push\("\/discover\/campus-modpacks"\)/);
});

test("the sync page lists instances without the campus modpack section", () => {
  const page = read("src/pages/instances/sync.tsx");
  assert.match(page, /<InstanceLedger/);
  assert.doesNotMatch(page, /CampusPlatformSection/);
  assert.doesNotMatch(page, /CampusModpackService/);

  const ledger = read("src/components/sync/instance-ledger.tsx");
  assert.doesNotMatch(ledger, /InstanceSyncPage\.ledger\.title/);
  assert.match(ledger, /InstanceSyncPage\.ledger\.checkAll/);
  assert.match(ledger, /InstanceSyncPage\.ledger\.installMore/);
  assert.match(ledger, /router\.push\("\/discover\/modpacks"\)/);
});

test("the campus section no longer renders its own title and description", () => {
  const section = read("src/components/campus-platform-section.tsx");
  assert.doesNotMatch(section, /InstanceSyncPage\.platform\.title/);
  assert.doesNotMatch(section, /InstanceSyncPage\.platform\.description/);
  const page = read("src/pages/discover/campus-modpacks.tsx");
  assert.match(page, /DiscoverCampusModpacksPage\.title/);
});
