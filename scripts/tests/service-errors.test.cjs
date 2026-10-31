const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");
const ts = require("typescript");

function load(file, dependencies, extra = {}) {
  const exports = {};
  vm.runInNewContext(
    ts.transpileModule(
      fs.readFileSync(path.join(__dirname, "../../", file), "utf8"),
      {
        compilerOptions: { module: ts.ModuleKind.CommonJS },
      }
    ).outputText,
    { exports, require: (name) => dependencies[name], ...extra }
  );
  return exports;
}

test("service errors stay structured when native logging is unavailable", async () => {
  const failingLog = async () => {
    throw new Error("WebView unavailable");
  };
  const { logger } = load("src/utils/logging.ts", {
    "@tauri-apps/plugin-log": Object.fromEntries(
      ["debug", "error", "info", "trace", "warn"].map((name) => [
        name,
        failingLog,
      ])
    ),
  });
  const { responseHandler } = load("src/utils/response.ts", {
    i18next: { t: (key, options) => options?.defaultValue || key },
    "@/utils/env": { isDev: false },
    "@/utils/file-error": load("src/utils/file-error.ts", {}),
    "@/utils/logging": { logger },
  });
  const descriptor = {
    value: async () => {
      throw new Error("original failure");
    },
  };
  responseHandler("test")({}, "operation", descriptor);
  const response = await descriptor.value();
  assert.equal(response.status, "error");
  assert.match(response.details, /original failure/);
  const cyclic = {};
  cyclic.self = cyclic;
  await assert.doesNotReject(logger.info(cyclic));
});

test("global logger setup works when a window already has a log property", () => {
  const window = { log: () => {} };
  const { setupLogger, logger } = load(
    "src/utils/logging.ts",
    {
      "@tauri-apps/plugin-log": {},
    },
    { window }
  );
  setupLogger();
  assert.equal(window.logger, logger);
});
