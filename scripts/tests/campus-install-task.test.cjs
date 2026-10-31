const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function load(file, requireModule = () => ({})) {
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
          jsx: ts.JsxEmit.React,
        },
      }
    ).outputText
  )(exports, module, requireModule);
  return module.exports;
}

const fileError = load("src/utils/file-error.ts");

const {
  campusInstallCardState,
  campusInstallStageKey,
  campusInstallProgressView,
  visibleCampusInstalls,
  localizeCampusInstallError,
  removeCampusInstallGroups,
  compareCampusInstallStages,
  campusInstallTaskIndeterminate,
  campusInstallGroupLabel,
  campusInstallGroupInstanceId,
  canDiscardRunningInstall,
} = load("src/utils/campus-install-task.ts", (name) =>
  name === "@/utils/file-error" ? fileError : {}
);

test("campus install cards expose active, paused, complete and failure states", () => {
  assert.equal(campusInstallCardState("Started"), "active-install");
  assert.equal(campusInstallCardState("InProgress"), "active-install");
  assert.equal(campusInstallCardState("Stopped"), "paused");
  assert.equal(campusInstallCardState("Completed"), "completed");
  assert.equal(campusInstallCardState("Failed"), "failed");
  assert.equal(campusInstallCardState("Started", "NETWORK_ERROR"), "failed");
  assert.equal(campusInstallCardState("Cancelled"), "cancelled");
});

test("campus install stage labels are normalized for locale lookup", () => {
  assert.equal(campusInstallStageKey("  Fetching "), "fetching");
  assert.equal(campusInstallStageKey("finalizing"), "applying");
  assert.equal(campusInstallStageKey("WaitingClient"), "waitingclient");
});

test("cards show verified file and byte counts only in the fetching stage", () => {
  const progress = {
    stage: "fetching",
    done: 12,
    total: 20,
    bytesDone: 120,
    bytesTotal: 200,
  };
  assert.deepEqual(campusInstallProgressView(progress), {
    stage: "fetching",
    done: 12,
    total: 20,
    bytes: { done: 120, total: 200 },
  });
  for (const stage of [
    "waitingClient",
    "preparing",
    "locating",
    "applying",
    "finalizing",
  ]) {
    assert.equal(campusInstallProgressView({ ...progress, stage }).bytes, null);
  }
  assert.equal(campusInstallProgressView(null), null);
});

test("resume and discard records remain visible at the top even with a failed group", () => {
  const jobs = [
    { instanceId: "a", group: "modpack-install?a", resumable: true },
    { instanceId: "b", group: "modpack-install?b", resumable: true },
    { instanceId: "c", group: null, resumable: true },
  ];
  assert.deepEqual(visibleCampusInstalls(jobs), jobs);
});

test("repeated resumes replace old groups, leaving one install card per instance", () => {
  const other = { taskGroup: "game-client?other@1" };
  let tasks = [other];
  let previous;
  for (let attempt = 1; attempt <= 4; attempt++) {
    const group = `modpack-install?航空学 [APP_DATA_SUBDIR:campus]@${attempt}`;
    tasks = removeCampusInstallGroups(tasks, previous ? [previous] : []);
    tasks = [{ taskGroup: group, status: "Started", taskDescs: [] }, ...tasks];
    assert.equal(
      tasks.filter((task) => task.taskGroup.startsWith("modpack-install?"))
        .length,
      1
    );
    assert.ok(tasks.includes(other));
    tasks[0].status = "Failed";
    previous = group;
  }
  assert.deepEqual(removeCampusInstallGroups(tasks, [previous]), [other]);
});

test("task context applies removal events before repeated resume creation and keeps stage order", () => {
  const states = [];
  const effects = [];
  const listeners = {};
  const react = {
    createContext: () => ({ Provider: "provider" }),
    createElement: () => null,
    useCallback: (fn) => fn,
    useRef: (current) => ({ current }),
    useState: (initial) => {
      const index = states.length;
      states.push(initial);
      return [
        initial,
        (value) => {
          states[index] =
            typeof value === "function" ? value(states[index]) : value;
        },
      ];
    },
    useEffect: (fn) => effects.push(fn),
  };
  const subscribe = (name) => (callback) => {
    listeners[name] = callback;
    return () => {};
  };
  const taskModel = load("src/models/task.ts");
  const context = load("src/contexts/task.tsx", (name) => {
    if (name === "react") return { ...react, default: react };
    if (name === "next/router") return { useRouter: () => ({}) };
    if (name === "@chakra-ui/react")
      return { useToast: () => ({ close: () => {} }) };
    if (name === "react-i18next")
      return { useTranslation: () => ({ t: (key) => key }) };
    if (name === "@/contexts/config") return { useLauncherConfig: () => ({}) };
    if (name === "@/contexts/global-data") return { useGlobalData: () => ({}) };
    if (name === "@/contexts/shared-modal")
      return { useSharedModals: () => ({}) };
    if (name === "@/contexts/toast") return { useToast: () => () => {} };
    if (name === "@/models/task") return taskModel;
    if (name === "@/utils/campus-install-task")
      return load("src/utils/campus-install-task.ts", (inner) =>
        inner === "@/utils/file-error" ? fileError : {}
      );
    if (name === "@/utils/modpack-update-task")
      return {
        isModpackUpdateGroup: (group) => group.startsWith("modpack-update?"),
      };
    if (name === "@/services/task")
      return {
        TaskService: {
          onProgressiveTaskUpdate: subscribe("task"),
        },
      };
    if (name === "@/services/campus-modpack")
      return {
        CampusModpackService: {
          onInstallGroupsRemoved: subscribe("removed"),
          onInstallProgress: subscribe("progress"),
        },
      };
    return {};
  });
  context.TaskContextProvider({ children: null });
  // Run only these subscriptions: timers, toasts and native invocations are
  // deliberately outside this unit test of the real event/state handlers.
  effects
    .filter((fn) =>
      /onProgressiveTaskUpdate|onInstallGroupsRemoved|onInstallProgress/.test(
        fn.toString()
      )
    )
    .forEach((fn) => fn());
  let previous;
  for (let attempt = 1; attempt <= 4; attempt++) {
    const group = `modpack-install?航空学 [games:a]@${attempt}`;
    listeners.removed(previous ? [previous] : []);
    for (const [index, filename] of [
      "Preparing",
      "Locating",
      "Fetching",
      "WaitingClient",
      "Applying",
    ].entries()) {
      const id = attempt * 10 + index;
      listeners.task({
        id,
        taskGroup: group,
        event: {
          status: "Created",
          desc: {
            taskId: id,
            status: "Waiting",
            current: 0,
            total: 1,
            payload: { taskType: "upload", filename },
          },
        },
      });
    }
    assert.equal(states[0].length, 1);
    assert.equal(states[0][0].taskGroup, group);
    assert.equal(states[0][0].taskDescs.length, 5);
    assert.deepEqual(
      states[0][0].taskDescs.map((task) => task.payload.filename),
      ["Preparing", "Locating", "Fetching", "WaitingClient", "Applying"]
    );
    listeners.progress({ group, stage: "locating" });
    assert.deepEqual(Object.keys(states[1]), [group]);
    previous = group;
  }
});

test("resume and discard controls render once outside the task group cards", () => {
  const source = ts.createSourceFile(
    "downloads.tsx",
    fs.readFileSync(
      path.join(__dirname, "../../src/pages/downloads.tsx"),
      "utf8"
    ),
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX
  );
  const controls = [];
  function visit(node) {
    if (
      ts.isJsxSelfClosingElement(node) &&
      node.tagName.getText(source) === "UnfinishedInstalls"
    ) {
      controls.push(node);
    }
    ts.forEachChild(node, visit);
  }
  visit(source);
  assert.equal(controls.length, 1);
  assert.equal(controls[0].attributes.properties.length, 0);
  assert.ok(ts.isJsxElement(controls[0].parent));
  assert.equal(
    controls[0].parent.openingElement.tagName.getText(source),
    "VStack"
  );
  for (let node = controls[0].parent; node; node = node.parent) {
    assert.ok(
      !ts.isCallExpression(node) ||
        !node.expression.getText(source).endsWith(".map")
    );
  }
});

test("install stages keep execution order regardless of status and insertion order", () => {
  const stages = [
    "Applying",
    "WaitingClient",
    "Locating",
    "Fetching",
    "Preparing",
  ].map((filename, index) => ({
    payload: { filename },
    status: index ? "Completed" : "Failed",
  }));
  assert.deepEqual(
    stages
      .sort(compareCampusInstallStages)
      .map((task) => task.payload.filename),
    ["Preparing", "Locating", "Fetching", "WaitingClient", "Applying"]
  );
});

test("unstarted install stages never animate, only an active stage without a total does", () => {
  for (const filename of [
    "Preparing",
    "Locating",
    "Fetching",
    "WaitingClient",
    "Applying",
  ]) {
    for (const total of [0, 1]) {
      assert.equal(
        campusInstallTaskIndeterminate({
          payload: { filename },
          status: "Waiting",
          total,
        }),
        false
      );
      assert.equal(
        campusInstallTaskIndeterminate({
          payload: { filename },
          status: "InProgress",
          total,
        }),
        total === 0
      );
    }
  }
});

test("install group titles hide internal identity but keep the human label", () => {
  assert.equal(
    campusInstallGroupLabel(
      "modpack-install?航空学 [APP_DATA_SUBDIR:campus]@123"
    ),
    "航空学"
  );
  assert.equal(
    campusInstallGroupLabel("modpack-install?同名 [自定义] [games:a]@123"),
    "同名 [自定义]"
  );
  assert.equal(campusInstallGroupLabel("modpack-install?航空学@123"), "航空学");
});

test("install group titles use the known instance id when it contains brackets", () => {
  const id = "APP_DATA_SUBDIR:Pack [Fabric] 1.8.2";
  const group = `modpack-install?Pack [Fabric] 1.8.2 [${id}]@123`;
  const known = ["games:Other", id];
  assert.equal(campusInstallGroupLabel(group, known), "Pack [Fabric] 1.8.2");
  assert.equal(campusInstallGroupInstanceId(group, known), id);

  // An id with "[" and characters the backend replaces in group names.
  const odd = "games:A [x?y@z";
  const oddGroup = "modpack-install?Title [games:A [x y z]@9";
  assert.equal(campusInstallGroupLabel(oddGroup, [odd]), "Title");
  assert.equal(campusInstallGroupInstanceId(oddGroup, [odd]), odd);

  // The longest matching id wins over a shorter one that ends the same way.
  const longer = "games:Pack [Fabric]";
  const shorter = "Fabric]";
  const both = `modpack-install?Pack [${longer}]@1`;
  assert.equal(campusInstallGroupInstanceId(both, [shorter, longer]), longer);
  assert.equal(campusInstallGroupLabel(both, [shorter, longer]), "Pack");
});

test("install group titles fall back to splitting when the id is unknown", () => {
  const group = "modpack-install?Pack [games:Pack]@123";
  assert.equal(campusInstallGroupLabel(group, ["games:Other"]), "Pack");
  assert.equal(
    campusInstallGroupInstanceId(group, ["games:Other"]),
    "games:Pack"
  );
  // Groups from before the id was written have nothing to split.
  assert.equal(
    campusInstallGroupLabel("modpack-install?Pack@123", ["x"]),
    "Pack"
  );
  assert.equal(
    campusInstallGroupInstanceId("modpack-install?Pack@123", ["x"]),
    null
  );
});

test("abandoning finds an install whose instance id contains brackets", () => {
  const id = "APP_DATA_SUBDIR:Pack [Fabric] 1.8.2";
  const group = `modpack-install?Pack [Fabric] 1.8.2 [${id}]@1`;
  const card = (descs) => ({
    taskGroup: group,
    status: "Started",
    taskDescs: descs,
  });
  const applying = [
    { payload: { filename: "Applying" }, status: "InProgress" },
  ];
  assert.equal(canDiscardRunningInstall(id, [card(applying)], {}), false);
  assert.equal(canDiscardRunningInstall(id, [card([])], {}), true);
  assert.equal(canDiscardRunningInstall(id, [card(applying)], {}, [id]), false);
});

test("failure reasons use locale keys and unknown errors never leak raw codes", () => {
  const t = (key, options) =>
    key.includes("MODPACK_UPDATE_VERIFY_FAILED")
      ? "校验失败"
      : (options?.defaultValue ?? key);
  assert.equal(
    localizeCampusInstallError(t, "MODPACK_UPDATE_VERIFY_FAILED"),
    "校验失败"
  );
  assert.match(
    localizeCampusInstallError(t, "http status 503"),
    /NETWORK_ERROR$/
  );
  assert.match(
    localizeCampusInstallError(t, "unknown raw failure"),
    /UNKNOWN_ERROR$/
  );
});

test("unexpected task termination has a localized failure reason in every locale", () => {
  for (const locale of ["en", "fr", "ja", "zh-Hans", "zh-Hant"]) {
    const messages = JSON.parse(
      fs.readFileSync(
        path.join(__dirname, "../../src/locales", `${locale}.json`),
        "utf8"
      )
    );
    const t = (key, options) =>
      key.split(".").reduce((value, part) => value?.[part], messages) ??
      options?.defaultValue ??
      key;
    const reason = localizeCampusInstallError(t, "MODPACK_INSTALL_TASK_FAILED");
    assert.equal(
      reason,
      messages.Services.instance.installCampusModpack.error.description
        .MODPACK_INSTALL_TASK_FAILED
    );
    assert.ok(reason.length > 0);
    assert.doesNotMatch(reason, /MODPACK_INSTALL_TASK_FAILED/);
    assert.equal(
      campusInstallCardState("Failed", "MODPACK_INSTALL_TASK_FAILED"),
      "failed"
    );
  }
});

test("a running install can be abandoned until it starts applying", () => {
  const group = "modpack-install?Pack [games:Pack]@1";
  const task = (filename, status) => ({ payload: { filename }, status });
  const card = (status, descs) => ({
    taskGroup: group,
    status,
    taskDescs: descs,
  });
  const waiting = [task("Fetching", "InProgress"), task("Applying", "Waiting")];
  assert.equal(campusInstallGroupInstanceId(group), "games:Pack");
  assert.equal(campusInstallGroupInstanceId("game-client?Pack@1"), null);
  assert.equal(
    canDiscardRunningInstall("games:Pack", [card("Started", waiting)], {}),
    true
  );
  // The progress event is the earlier signal.
  assert.equal(
    canDiscardRunningInstall("games:Pack", [card("Started", waiting)], {
      [group]: { instanceId: "games:Pack", stage: "applying" },
    }),
    false
  );
  assert.equal(
    canDiscardRunningInstall("games:Pack", [card("Started", waiting)], {
      [group]: { instanceId: "games:Pack", stage: "finalizing" },
    }),
    false
  );
  assert.equal(
    canDiscardRunningInstall(
      "games:Pack",
      [card("Started", [task("Applying", "InProgress")])],
      {}
    ),
    false
  );
  // Another instance's install, or a card that is not running, does not count.
  assert.equal(
    canDiscardRunningInstall(
      "games:Other",
      [card("Started", [task("Applying", "InProgress")])],
      {}
    ),
    true
  );
  assert.equal(
    canDiscardRunningInstall(
      "games:Pack",
      [card("Failed", [task("Applying", "Failed")])],
      {}
    ),
    true
  );
});
