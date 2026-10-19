const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");
const ts = require("typescript");
const exportsObject = {};
vm.runInNewContext(
  ts.transpileModule(
    fs.readFileSync(
      path.join(__dirname, "../../src/utils/subscription.ts"),
      "utf8"
    ),
    { compilerOptions: { module: ts.ModuleKind.CommonJS } }
  ).outputText,
  {
    exports: exportsObject,
    require: () => ({ logger: { warn: async () => {} } }),
  }
);
const { subscribeAsync } = exportsObject;
const tick = () => new Promise(setImmediate);

test("late listener registration is disposed once and cannot deliver stale events", async () => {
  let resolve;
  let callback;
  let cleanups = 0;
  const events = [];
  const registration = new Promise((complete) => {
    resolve = complete;
  });
  const dispose = subscribeAsync(
    (handler) => {
      callback = handler;
      return registration;
    },
    (event) => events.push(event)
  );
  await tick();
  callback("active");
  dispose();
  dispose();
  callback("stale");
  resolve(() => {
    cleanups++;
  });
  await tick();
  assert.equal(cleanups, 1);
  assert.deepEqual(events, ["active"]);
});

test("registration and asynchronous cleanup failures are handled", async () => {
  const errors = [];
  subscribeAsync(
    async () => {
      throw new Error("register");
    },
    () => {},
    (error) => errors.push(error.message)
  );
  const dispose = subscribeAsync(
    async () => async () => {
      throw new Error("cleanup");
    },
    () => {},
    (error) => errors.push(error.message)
  );
  await tick();
  dispose();
  await tick();
  assert.deepEqual(errors, ["register", "cleanup"]);
});
