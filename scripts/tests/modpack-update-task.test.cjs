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
    "React",
    ts.transpileModule(
      fs.readFileSync(path.join(__dirname, "../../", file), "utf8"),
      {
        compilerOptions: {
          module: ts.ModuleKind.CommonJS,
          jsx: ts.JsxEmit.React,
        },
      }
    ).outputText
  )(module.exports, module, requireModule, { createElement: () => null });
  return module.exports;
}

const choices = load("src/utils/modpack-update-choices.ts");
const stages = load("src/utils/campus-install-task.ts");
const update = load("src/utils/modpack-update-task.ts", (name) =>
  name.endsWith("modpack-update-choices") ? choices : stages
);

test("update groups are separate from installs, have ordered stages and a clean title", () => {
  const group = "modpack-update?航空学 [APP_DATA_SUBDIR:test]@1";
  assert.equal(update.isModpackUpdateGroup(group), true);
  assert.equal(stages.isCampusInstallGroup(group), false);
  assert.equal(stages.campusInstallGroupLabel(group), "航空学");
  assert.deepEqual(
    ["Applying", "Fetching", "Preparing", "Locating"]
      .map((filename) => ({ payload: { filename } }))
      .sort(stages.compareCampusInstallStages)
      .map((task) => task.payload.filename),
    ["Preparing", "Locating", "Fetching", "Applying"]
  );
});

test("update failure and panic reasons are readable in all five locales", () => {
  for (const locale of ["en", "fr", "ja", "zh-Hans", "zh-Hant"]) {
    const messages = JSON.parse(
      fs.readFileSync(
        path.join(__dirname, "../../src/locales", `${locale}.json`),
        "utf8"
      )
    );
    const t = (key, options) =>
      key.split(".").reduce((v, part) => v?.[part], messages) ??
      options?.defaultValue ??
      key;
    for (const code of [
      "MODPACK_UPDATE_TASK_FAILED",
      "MODPACK_UPDATE_VERIFY_FAILED",
      "MODPACK_UPDATE_STALE",
      "http status 503",
      "unrecognized failure",
    ]) {
      const text = update.localizeModpackUpdateError(t, code);
      assert.ok(text.length > 0);
      assert.notEqual(text, code);
      assert.doesNotMatch(text, /MODPACK_|Services\./);
    }
  }
});

test("launch busy errors are localized by the actual response handler and displayed by the launch dialog", async () => {
  for (const locale of ["en", "fr", "ja", "zh-Hans", "zh-Hant"]) {
    const messages = JSON.parse(
      fs.readFileSync(
        path.join(__dirname, "../../src/locales", `${locale}.json`),
        "utf8"
      )
    );
    const { responseHandler } = load("src/utils/response.ts", (name) => {
      if (name === "i18next")
        return {
          t: (key, options) =>
            key.split(".").reduce((v, part) => v?.[part], messages) ??
            options?.defaultValue ??
            key,
        };
      if (name === "@/utils/env") return { isDev: false };
      if (name === "@/utils/logging") return { logger: { error: () => {} } };
      return {};
    });
    const descriptor = {
      value: async () => {
        throw "MODPACK_UPDATE_BUSY";
      },
    };
    responseHandler("launch")({}, "selectSuitableJRE", descriptor);
    const result = await descriptor.value();
    assert.equal(result.status, "error");
    assert.equal(result.raw_error, "MODPACK_UPDATE_BUSY");
    assert.equal(
      result.details,
      messages.Services.launch.selectSuitableJRE.error.description
        .MODPACK_UPDATE_BUSY
    );
    assert.ok(result.details.length > 0);
    assert.doesNotMatch(result.details, /MODPACK_|Services\./);
  }
  const modal = fs.readFileSync(
    path.join(__dirname, "../../src/components/modals/launch-process-modal.tsx"),
    "utf8"
  );
  assert.match(modal, /setErrorDesc\(response\.details\)/);
  assert.match(modal, /\{errorDesc\}/);
});

test("launch checks registered update jobs inside UpdateGuard and updates retain engine staging reuse", () => {
  const read = (file) =>
    fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
  const launch = read("src-tauri/src/launch/commands.rs");
  const step = launch.slice(
    launch.indexOf("pub async fn select_suitable_jre"),
    launch.indexOf("// Step 2:")
  );
  assert.ok(
    step.indexOf("UpdateGuard::while_idle") <
      step.indexOf(".while_no_modpack_update")
  );
  assert.ok(
    step.indexOf(".while_no_modpack_update(&instance_id") <
      step.indexOf("launching.push")
  );
  const commands = read("src-tauri/src/instance/modpack_update/commands.rs");
  const apply = commands.slice(
    commands.indexOf("pub async fn apply_modpack_update("),
    commands.indexOf("/// The modpack version the launcher already announced")
  );
  assert.match(apply, /register_modpack_update_group\(\s*&instance_id,/);
  assert.ok(
    apply.indexOf("register_modpack_update_group") <
      apply.indexOf("tauri::async_runtime::spawn")
  );
  assert.doesNotMatch(commands, /discard_update_staging/);
  const engine = read("src-tauri/src/instance/modpack_update/apply.rs");
  assert.doesNotMatch(engine, /discard_update_staging/);
  assert.match(
    engine,
    /meta\.version_id == request\.remote\.version_id && meta\.plan_hash == request\.plan_hash/
  );
});

test("global task context receives results with no update modal mounted", () => {
  const effects = [];
  const states = [];
  const notices = [];
  const checks = [];
  const routes = [];
  let refreshes = 0;
  let finished;
  const react = {
    createContext: () => ({ Provider: "provider" }),
    createElement: () => null,
    useCallback: (fn) => fn,
    useRef: (current) => ({ current }),
    useEffect: (fn) => effects.push(fn),
    useState: (initial) => {
      const index = states.push(initial) - 1;
      return [
        initial,
        (value) => {
          states[index] =
            typeof value === "function" ? value(states[index]) : value;
        },
      ];
    },
  };
  const context = load("src/contexts/task.tsx", (name) => {
    if (name === "react") return { ...react, default: react };
    if (name === "next/router")
      return { useRouter: () => ({ push: (route) => routes.push(route) }) };
    if (name === "@chakra-ui/react")
      return { useToast: () => ({ close: () => {} }) };
    if (name === "react-i18next")
      return { useTranslation: () => ({ t: (key) => key }) };
    if (name === "@/contexts/config") return { useLauncherConfig: () => ({}) };
    if (name === "@/contexts/global-data")
      return {
        useGlobalData: () => ({
          getInstanceList: () => {
            refreshes++;
          },
          checkModpackUpdate: (...args) => checks.push(args),
        }),
      };
    if (name === "@/contexts/shared-modal")
      return { useSharedModals: () => ({}) };
    if (name === "@/contexts/toast")
      return { useToast: () => (notice) => notices.push(notice) };
    if (name === "@/utils/modpack-update-task") return update;
    if (name === "@/services/modpack-update")
      return {
        ModpackUpdateService: {
          onUpdateFinished: (callback) => {
            finished = callback;
            return () => {};
          },
        },
      };
    return {};
  });
  context.TaskContextProvider({ children: null });
  effects
    .filter((fn) => fn.toString().includes("onUpdateFinished"))
    .forEach((fn) => fn());
  assert.equal(typeof finished, "function");
  const answer = { "config/test.json#0": "takeRemote" };
  choices.rememberedChoices.set("test", { planHash: "hash", choices: answer });
  const payload = {
    instanceId: "test",
    instanceName: "测试",
    group: "modpack-update?test@1",
    planHash: "hash",
    outcome: null,
    error: "MODPACK_UPDATE_VERIFY_FAILED",
  };
  finished(payload);
  assert.equal(refreshes, 1);
  assert.equal(notices.at(-1).status, "error");
  assert.equal(choices.failedUpdatePlans.get("test"), "hash");
  assert.ok(states.some((state) => state?.[payload.group] === payload));
  const restored = choices.resolvePlanChoices({
    instanceId: "test",
    newPlanHash: "hash",
    failedPlanHash: choices.failedUpdatePlans.get("test"),
    recheckAfterFailure: true,
    remembered: choices.rememberedChoices,
    currentChoices: answer,
  });
  assert.deepEqual(restored.choices, answer);
  const changed = choices.resolvePlanChoices({
    instanceId: "test",
    newPlanHash: "other",
    failedPlanHash: choices.failedUpdatePlans.get("test"),
    recheckAfterFailure: true,
    remembered: choices.rememberedChoices,
    currentChoices: answer,
  });
  assert.deepEqual(changed, { choices: {}, toastPlanChanged: true });
  finished({
    ...payload,
    error: null,
    outcome: { versionId: "next", loaderInstallPending: true },
  });
  assert.equal(refreshes, 2);
  assert.deepEqual(checks, [["test", true]]);
  assert.deepEqual(routes, ["/downloads"]);
  assert.equal(choices.rememberedChoices.has("test"), false);
  assert.equal(choices.failedUpdatePlans.has("test"), false);
  assert.equal(notices.at(-2).status, "success");
  assert.equal(notices.at(-1).status, "info");
});

test("failed update cards reopen the planner, never offer download retry or install continuation", () => {
  const source = fs.readFileSync(
    path.join(__dirname, "../../src/pages/downloads.tsx"),
    "utf8"
  );
  assert.match(source, /updateResults\[group.taskGroup\]\?\.error/);
  assert.match(source, /openSharedModal\("update-modpack",/);
  assert.match(source, /DownloadTasksPage.recheckUpdate/);
  assert.match(source, /!isModpackTransferGroup\(group.taskGroup\)/);
  const modal = fs.readFileSync(
    path.join(
      __dirname,
      "../../src/components/modals/update-modpack-modal.tsx"
    ),
    "utf8"
  );
  const apply = modal.slice(
    modal.indexOf("const handleApply"),
    modal.indexOf("const handleRollback")
  );
  assert.match(apply, /modalProps.onClose\(\)/);
  assert.doesNotMatch(
    apply,
    /loaderInstallPending|checkModpackUpdate\(instanceId, true\)/
  );
});

test("reopened dialog restores a failed plan's choices, submits once and closes on registration", async () => {
  for (const samePlan of [true, false]) {
    choices.rememberedChoices.clear();
    choices.failedUpdatePlans.clear();
    const answer = { "config/test.json#0": "takeRemote" };
    choices.rememberedChoices.set("test", {
      planHash: "failed",
      choices: answer,
    });
    choices.failedUpdatePlans.set("test", "failed");
    const slots = [];
    let cursor = 0;
    let callbacks = [];
    let closed = 0;
    const submitted = [];
    const notifications = [];
    const react = {
      useCallback: (fn) => {
        callbacks.push(fn);
        return fn;
      },
      useEffect: () => {},
      useRef: (initial) => {
        const i = cursor++;
        return (slots[i] ??= { current: initial });
      },
      useState: (initial) => {
        const i = cursor++;
        if (!(i in slots)) slots[i] = initial;
        return [
          slots[i],
          (value) => {
            slots[i] = typeof value === "function" ? value(slots[i]) : value;
          },
        ];
      },
    };
    const plan = {
      planHash: samePlan ? "failed" : "changed",
      fastForward: true,
      conflicts: [],
      changes: [],
      toLabel: "next",
    };
    const modal = load(
      "src/components/modals/update-modpack-modal.tsx",
      (name) => {
        if (name === "react") return react;
        if (name === "react-i18next")
          return { useTranslation: () => ({ t: (key) => key }) };
        if (name === "@/contexts/config")
          return {
            useLauncherConfig: () => ({
              config: { appearance: { theme: {} } },
            }),
          };
        if (name === "@/contexts/toast")
          return { useToast: () => (notice) => notifications.push(notice) };
        if (name === "@/contexts/global-data")
          return {
            useGlobalData: () => ({
              getInstanceList: () => [],
              checkModpackUpdate: async () => ({ status: "available" }),
            }),
          };
        if (name === "@/utils/modpack-update-choices") return choices;
        if (name === "@/enums/service-error") return { InstanceError: {} };
        if (name === "@/services/modpack-update")
          return {
            ModpackUpdateService: {
              recoverModpackUpdate: async () => ({
                status: "success",
                data: false,
              }),
              prepareModpackUpdate: async () => ({
                status: "success",
                data: plan,
              }),
              applyModpackUpdate: async (...args) => {
                submitted.push(args);
                return {
                  status: "success",
                  data: { group: "modpack-update?test@1" },
                };
              },
            },
          };
        return {};
      }
    );
    const render = () => {
      cursor = 0;
      callbacks = [];
      modal.default({
        instanceId: "test",
        instanceName: "test",
        isOpen: true,
        onClose: () => closed++,
      });
    };
    render();
    await callbacks.find((fn) =>
      fn.toString().includes("recoverModpackUpdate")
    )();
    render();
    const apply = callbacks.find((fn) =>
      fn.toString().includes("applyModpackUpdate")
    );
    await Promise.all([apply(), apply()]);
    assert.deepEqual(submitted, [
      ["test", plan.planHash, samePlan ? answer : {}],
    ]);
    assert.equal(closed, 1);
    assert.equal(
      notifications.some(
        (notice) => notice.title === "UpdateModpackModal.toast.planChanged"
      ),
      !samePlan
    );
    // Registration closes the dialog, not a final success notification.
    assert.equal(notifications.at(-1).title, "DownloadTasksPage.updateStarted");
    assert.equal(choices.rememberedChoices.get("test").planHash, plan.planHash);
  }
  choices.rememberedChoices.clear();
  choices.failedUpdatePlans.clear();
});
