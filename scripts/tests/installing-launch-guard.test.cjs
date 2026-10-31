const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

const read = (file) =>
  fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
function load(file, requireModule) {
  const module = { exports: {} };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(read(file), {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
      },
    }).outputText
  )(module.exports, module, requireModule);
  return module.exports;
}
const helpers = load("src/utils/installing-instance.ts", () => ({}));
const buttons = load("src/components/instance-launch-button.tsx", (name) => {
  if (name === "react/jsx-runtime")
    return { jsx: (type, props) => ({ type, props }) };
  if (name === "@chakra-ui/react")
    return { Button: "Button", Tooltip: "Tooltip" };
  if (name === "react-i18next")
    return { useTranslation: () => ({ t: (key) => key }) };
  if (name === "@/utils/installing-instance") return helpers;
  return { CommonIconButton: "CommonIconButton" };
});

for (const name of ["InstanceLaunchButton", "InstanceLaunchIconButton"]) {
  test(`${name} disables unfinished installs with a reason and unlocks on refreshed summaries`, () => {
    const onClick = () => {};
    const render = (instance, isDisabled = false) =>
      buttons[name]({ instance, isDisabled, onClick, children: "Launch" });
    const pending = { campusInstallPending: true };
    const blocked = render(pending);
    assert.equal(blocked.props.children.props.isDisabled, true);
    assert.equal(blocked.type, "Tooltip");
    assert.equal(blocked.props.shouldWrapChildren, true);
    assert.match(blocked.props.label, /LAUNCH_INSTANCE_INSTALLING$/);
    assert.equal(blocked.props.children.props.onClick, onClick);
    for (const instance of [
      undefined,
      {},
      { ...pending, campusInstallPending: false },
    ]) {
      const enabled = render(instance);
      assert.equal(enabled.props.isDisabled, false);
    }
    assert.equal(render({}, true).props.isDisabled, true);
  });
}

test("non-installing launch controls render the original button without an extra tooltip wrapper", () => {
  const onClick = () => {};
  for (const [name, type] of [
    ["InstanceLaunchButton", "Button"],
    ["InstanceLaunchIconButton", "CommonIconButton"],
  ]) {
    for (const instance of [undefined, {}, { campusInstallPending: false }]) {
      const props = { onClick, withTooltip: false, isDisabled: false };
      const rendered = buttons[name]({ instance, ...props });
      assert.equal(rendered.type, type);
      assert.deepEqual(rendered.props, props);
    }
  }
});

const entryPoints = [
  ["src/pages/launch.tsx", "InstanceLaunchButton", "selectedInstance", 1],
  [
    "src/pages/instances/list/[[...dir]].tsx",
    "InstanceLaunchButton",
    "selectedInstance",
    1,
  ],
  [
    "src/layouts/instance-details-layout.tsx",
    "InstanceLaunchButton",
    "summary",
    1,
  ],
  ["src/components/instance-widgets.tsx", "InstanceLaunchButton", "summary", 1],
  [
    "src/pages/instances/details/[id]/worlds.tsx",
    "InstanceLaunchIconButton",
    "summary",
    2,
  ],
];
for (const [file, button, instanceExpression, count] of entryPoints) {
  test(`${file} wires every launch control to the tested installing guard`, () => {
    const source = ts.createSourceFile(
      file,
      read(file),
      ts.ScriptTarget.Latest,
      true
    );
    const controls = [];
    const launchCalls = [];
    function visit(node) {
      if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) {
        if (node.tagName.getText(source) === button) controls.push(node);
      }
      if (
        ts.isCallExpression(node) &&
        node.expression.getText(source) === "openSharedModal" &&
        node.arguments[0]?.text === "launch"
      )
        launchCalls.push(node);
      ts.forEachChild(node, visit);
    }
    visit(source);
    assert.equal(controls.length, count);
    assert.equal(launchCalls.length, count);
    for (const control of controls) {
      const instance = control.attributes.properties.find(
        (attribute) => attribute.name?.text === "instance"
      );
      assert.equal(
        instance.initializer.expression.getText(source),
        instanceExpression
      );
      if (button === "InstanceLaunchIconButton") {
        const wrapped = control.parent;
        assert.ok(ts.isParenthesizedExpression(wrapped));
        const branch = wrapped.parent;
        assert.ok(ts.isConditionalExpression(branch));
        assert.equal(
          branch.condition.getText(source),
          'item.icon === "launch"'
        );
        assert.equal(branch.whenTrue.expression, control);
        assert.equal(
          branch.whenFalse.expression.tagName.getText(source),
          "CommonIconButton"
        );
      }
    }
    if (button === "InstanceLaunchButton") {
      for (const call of launchCalls) {
        let parent = call.parent;
        while (parent && !ts.isJsxElement(parent)) parent = parent.parent;
        assert.equal(parent?.openingElement.tagName.getText(source), button);
      }
    }
  });
}

test("both list and grid cards remain selection-only and use the guarded selected-instance launch", () => {
  const source = read("src/components/instances-view.tsx");
  assert.match(
    source,
    /onClick: \(\) => handleUpdateSelectedInstance\(instance\)/
  );
  assert.match(
    source,
    /onSelect: \(\) => handleUpdateSelectedInstance\(instance\)/
  );
  assert.doesNotMatch(source, /openSharedModal\("launch"/);
  assert.doesNotMatch(
    read("src/components/instance-menu.tsx"),
    /openSharedModal\("launch"/
  );
});

test("tray requests and desktop quick launches still reach step-one backend rejection and readable toast", () => {
  assert.match(
    read("src/pages/tray-popup.tsx"),
    /triggerMain\("ustbl:tray-launch-request"\)/
  );
  const handler = read("src/components/special/global-event-handler.tsx");
  for (const event of ["ustbl:tray-launch", "ustbl:tray-launch-request"]) {
    assert.ok(handler.includes(`"${event}"`));
  }
  assert.match(
    handler,
    /openSharedModal\("launch", \{ instanceId: decodeId \}\)/
  );
  const modal = read("src/components/modals/launch-process-modal.tsx");
  assert.match(modal, /LaunchService.selectSuitableJRE\(instanceId\)/);
  assert.match(
    modal,
    /LaunchServiceError.LaunchInstanceInstalling\) \{\s*toast\(\{\s*title: error.details/
  );
  const rust = read("src-tauri/src/launch/commands.rs");
  assert.ok(
    rust.indexOf("let instance = launchable_instance(instance") <
      rust.indexOf("load_json_async::<McClientInfo>")
  );
});

test("all five locales provide the same readable reason for button hints and backend errors", () => {
  for (const locale of ["en", "fr", "ja", "zh-Hans", "zh-Hant"]) {
    const json = JSON.parse(read(`src/locales/${locale}.json`));
    const reason =
      json.Services.launch.selectSuitableJRE.error.description
        .LAUNCH_INSTANCE_INSTALLING;
    assert.equal(typeof reason, "string");
    assert.ok(reason.length > 30);
    assert.doesNotMatch(reason, /LAUNCH_INSTANCE_INSTALLING/);
  }
});

test("step-one backend installing rejection produces a readable toast", () => {
  const steps = [];
  const toasts = [];
  const react = {
    useState: () => [undefined, () => {}],
    useRef: () => ({ current: -1 }),
    useEffect: () => {},
    useCallback: (fn) => fn,
    useMemo: (factory) => {
      const value = factory();
      steps.push(...value);
      return value;
    },
  };
  const modal = load(
    "src/components/modals/launch-process-modal.tsx",
    (name) => {
      if (name === "react") return react;
      if (name === "react/jsx-runtime")
        return {
          jsx: (type, props) => ({ type, props }),
          jsxs: (type, props) => ({ type, props }),
        };
      if (name === "next/router") return { useRouter: () => ({}) };
      if (name === "react-i18next")
        return { useTranslation: () => ({ t: (key) => key }) };
      if (name === "@/contexts/config")
        return {
          useLauncherConfig: () => ({ config: { appearance: { theme: {} } } }),
        };
      if (name === "@/contexts/global-data")
        return { useGlobalData: () => ({}) };
      if (name === "@/contexts/shared-modal")
        return { useSharedModals: () => ({}) };
      if (name === "@/contexts/toast")
        return { useToast: () => (toast) => toasts.push(toast) };
      if (name === "@/enums/service-error")
        return {
          LaunchServiceError: {
            LaunchInstanceInstalling: "LAUNCH_INSTANCE_INSTALLING",
          },
        };
      return {};
    }
  );
  modal.default({ instanceId: "games:pending", onClose: () => {} });
  const details = JSON.parse(read("src/locales/zh-Hans.json")).Services.launch
    .selectSuitableJRE.error.description.LAUNCH_INSTANCE_INSTALLING;
  steps[0].onErrCallback({ raw_error: "LAUNCH_INSTANCE_INSTALLING", details });
  assert.deepEqual(toasts, [{ title: details, status: "warning" }]);
});
