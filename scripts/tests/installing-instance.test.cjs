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
      {
        compilerOptions: {
          module: ts.ModuleKind.CommonJS,
          jsx: ts.JsxEmit.React,
        },
      }
    ).outputText
  )(module.exports, module, requireModule);
  return module.exports;
}
const helpers = load("src/utils/installing-instance.ts");
const ledger = load("src/utils/modpack-ledger.ts", (name) =>
  name === "@/utils/installing-instance"
    ? helpers
    : { ModLoaderType: { Unknown: "Unknown" } }
);
const instance = (id, pending = false) => ({
  id,
  name: id,
  campusInstallPending: pending,
  origin: { platform: "campus", follow: true },
});

test("startup and reminders exclude every persisted installation, not WaitingLoader", () => {
  const pending = instance("pending", true);
  const installed = instance("installed");
  const waitingLoader = {
    ...instance("loader"),
    modLoader: { status: "Downloading" },
  };
  assert.deepEqual(
    [pending, installed, waitingLoader].filter(helpers.shouldCheckInstance),
    [installed, waitingLoader]
  );
  assert.equal(helpers.shouldCheckInstance(undefined), false);
  assert.equal(helpers.shouldCheckInstance({ id: "plain" }), false);
});

test("ledger shows awaiting installation and disables update even with stale available checks", () => {
  const pending = instance("pending", true);
  const stale = { status: "available", latestVersionId: "35" };
  assert.equal(ledger.ledgerBadge(pending, stale), "installing");
  assert.equal(ledger.ledgerUpdateDisabled(pending, stale), true);
  assert.equal(
    ledger.ledgerUpdateDisabled(instance("old"), { status: "installing" }),
    true
  );
  assert.equal(
    ledger.ledgerUpdateDisabled(instance("loader"), {
      status: "waitingLoader",
    }),
    false
  );
  assert.equal(
    ledger.ledgerBadge(instance("loader"), { status: "waitingLoader" }),
    "waitingLoader"
  );
});

test("navigation has disjoint groups and counts, and installing group disappears after success", () => {
  const all = [instance("a"), instance("b", true), instance("c", true)];
  const grouped = helpers.instanceNavigationGroups(all);
  assert.deepEqual(
    grouped.all.map((item) => item.id),
    ["a"]
  );
  assert.deepEqual(
    grouped.installing.map((item) => item.id),
    ["b", "c"]
  );
  assert.equal(grouped.allCount, 1);
  assert.equal(grouped.installingCount, 2);
  const completed = helpers.instanceNavigationGroups(
    all.map((item) => ({ ...item, campusInstallPending: false }))
  );
  assert.equal(completed.allCount, 3);
  assert.equal(completed.installingCount, 0);
  assert.equal(helpers.instanceNavigationGroups([]).allCount, 0);
});

test("global context skips pending startup calls and checks the successful install once", async () => {
  const states = [];
  const effects = [];
  const calls = [];
  const listeners = {};
  const instances = [instance("pending", true), instance("installed")];
  const react = {
    createContext: () => ({ Provider: "provider" }),
    createElement: (_component, props, ...children) => ({ ...props, children }),
    useCallback: (fn) => fn,
    useRef: (current) => ({ current }),
    useState: (initial) => {
      const index = states.length;
      const value = index === 2 ? instances : initial;
      states.push(value);
      return [
        value,
        (next) => {
          states[index] =
            typeof next === "function" ? next(states[index]) : next;
        },
      ];
    },
    useEffect: (fn) => effects.push(fn),
  };
  const provider = load("src/contexts/global-data.tsx", (name) => {
    if (name === "react") return { ...react, default: react };
    if (name === "react-i18next")
      return { useTranslation: () => ({ t: (key) => key }) };
    if (name === "@/contexts/config")
      return {
        useLauncherConfig: () => ({ config: { states: { shared: {} } } }),
      };
    if (name === "@/contexts/toast") return { useToast: () => () => {} };
    if (name === "@/hooks/get-state")
      return {
        useGetState: () => () => {},
        usePromisedGetState: () => [() => {}, false],
      };
    if (name === "@/utils/installing-instance") return helpers;
    if (name === "@/services/modpack-update")
      return {
        ModpackUpdateService: {
          checkModpackUpdate: async (id) => {
            calls.push(id);
            return { status: "success", data: { status: "upToDate" } };
          },
        },
      };
    if (name === "@/services/campus-modpack")
      return {
        CampusModpackService: {
          onInstallFinished: (callback) => {
            listeners.finished = callback;
            return () => {};
          },
        },
      };
    if (name === "@/services/instance")
      return {
        InstanceService: {
          retrieveInstanceList: async () => ({
            status: "success",
            data: instances.map((item) => ({
              ...item,
              campusInstallPending: false,
            })),
          }),
        },
      };
    return {};
  });
  const rendered = provider.GlobalDataContextProvider({ children: null });
  const pendingCheck = await rendered.value.checkModpackUpdate("pending", true);
  assert.equal(pendingCheck.status, "installing");
  assert.deepEqual(calls, []);
  effects
    .filter((fn) => /startupChecked|onInstallFinished/.test(fn.toString()))
    .forEach((fn) => fn());
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls, ["installed"]);
  listeners.finished({ instanceId: "pending", error: "NETWORK_ERROR" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls, ["installed"]);
  listeners.finished({ instanceId: "pending" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls, ["installed", "pending"]);
  assert.equal(
    states[2].find((item) => item.id === "pending").campusInstallPending,
    false
  );
});

test("views wire pending flags to ledger, both card styles, details and navigation", () => {
  const source = (file) =>
    fs.readFileSync(path.join(__dirname, "../../src", file), "utf8");
  assert.match(
    source("components/sync/instance-ledger.tsx"),
    /isDisabled=\{ledgerUpdateDisabled\(instance, check\)\}/
  );
  assert.equal(
    (
      source("components/instances-view.tsx").match(
        /AllInstancesPage.installing/g
      ) || []
    ).length,
    2
  );
  assert.match(
    source("layouts/instance-details-layout.tsx"),
    /isInstallingInstance\(summary\)/
  );
  assert.match(
    source("layouts/instance-details-layout.tsx"),
    /router.push\("\/downloads"\)/
  );
  assert.match(
    source("layouts/instances-layout.tsx"),
    /groups.installingCount > 0/
  );
  assert.match(
    source("layouts/instances-layout.tsx"),
    /instanceItems\(groups.all\)/
  );
  assert.match(
    source("layouts/instances-layout.tsx"),
    /instanceItems\(groups.installing\)/
  );
});
