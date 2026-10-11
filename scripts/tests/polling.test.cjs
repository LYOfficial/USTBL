const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");
const ts = require("typescript");

function loadPolling() {
  const exports = {};
  const timers = new Map();
  let nextId = 0;
  vm.runInNewContext(
    ts.transpileModule(
      fs.readFileSync(
        path.join(__dirname, "../../src/utils/polling.ts"),
        "utf8"
      ),
      { compilerOptions: { module: ts.ModuleKind.CommonJS } }
    ).outputText,
    {
      exports,
      require: () => ({ logger: { warn: async () => {} } }),
      setTimeout: (callback) => {
        const id = ++nextId;
        timers.set(id, callback);
        return id;
      },
      clearTimeout: (id) => timers.delete(id),
    }
  );
  return { pollAsync: exports.pollAsync, timers };
}

const tick = () => new Promise(setImmediate);

test("slow status requests never overlap and disposal rejects late results", async () => {
  const { pollAsync, timers } = loadPolling();
  let resolve;
  let requests = 0;
  const received = [];
  const dispose = pollAsync(
    () => {
      requests++;
      return new Promise((complete) => {
        resolve = complete;
      });
    },
    (value) => received.push(value),
    3000
  );
  await tick();
  assert.equal(requests, 1);
  assert.equal(timers.size, 0);
  resolve("first");
  await tick();
  assert.deepEqual(received, ["first"]);
  assert.equal(timers.size, 1);
  const [id, callback] = timers.entries().next().value;
  timers.delete(id);
  callback();
  assert.equal(requests, 2);
  dispose();
  resolve("stale");
  await tick();
  assert.deepEqual(received, ["first"]);
  assert.equal(timers.size, 0);
});

test("failed polling reports the error and retries until disposed", async () => {
  const { pollAsync, timers } = loadPolling();
  const errors = [];
  const dispose = pollAsync(
    async () => {
      throw new Error("offline");
    },
    () => assert.fail("Failed requests cannot deliver status"),
    3000,
    (error) => errors.push(error.message)
  );
  await tick();
  assert.deepEqual(errors, ["offline"]);
  assert.equal(timers.size, 1);
  dispose();
  dispose();
  assert.equal(timers.size, 0);
});
