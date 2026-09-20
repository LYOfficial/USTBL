const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");
const ts = require("typescript");

const source = fs.readFileSync(
  path.join(__dirname, "../../src/utils/skin-preview-resize.ts"),
  "utf8"
);
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS },
}).outputText;

function setup() {
  let now = 0;
  let sequence = 0;
  const timers = new Map();
  const exports = {};
  vm.runInNewContext(compiled, {
    exports,
    setTimeout(callback, delay) {
      const id = ++sequence;
      timers.set(id, { callback, due: now + delay });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
  });
  const calls = [];
  const viewer = {
    width: 300,
    height: 230,
    canvas: { style: {} },
    setSize(width, height) {
      this.width = width;
      this.height = height;
      this.canvas.style.width = `${width}px`;
      this.canvas.style.height = `${height}px`;
      calls.push(["setSize", width, height]);
    },
    render() {
      assert.equal(this.canvas.style.width, "auto");
      assert.equal(this.canvas.style.height, "100%");
      calls.push(["render"]);
    },
  };
  const resizer = exports.createSkinPreviewResizer(viewer);
  function advance(ms) {
    now += ms;
    for (const [id, timer] of timers) {
      if (timer.due <= now) {
        timers.delete(id);
        timer.callback();
      }
    }
  }
  return { resizer, viewer, calls, advance, timers };
}

test("initial size does not reallocate drawing buffers", () => {
  const { resizer, calls, advance } = setup();
  resizer.resize(300, 230);
  advance(1000);
  assert.deepEqual(calls, []);
});

test("continuous resizing retains the old buffer and settles once", () => {
  const { resizer, viewer, calls, advance } = setup();
  for (let width = 301; width <= 340; width++) {
    resizer.resize(width, 250);
    advance(10);
  }
  assert.equal(viewer.width, 300);
  assert.deepEqual(calls, []);
  advance(149);
  assert.deepEqual(calls, []);
  advance(1);
  assert.deepEqual(calls, [["setSize", 340, 250], ["render"]]);
});

test("returning to the existing size cancels a pending resize", () => {
  const { resizer, calls, advance } = setup();
  resizer.resize(400, 400);
  advance(50);
  resizer.resize(300, 230);
  advance(500);
  assert.deepEqual(calls, []);
});

test("unmount cancels the deferred resize and rejects later requests", () => {
  const { resizer, calls, advance, timers } = setup();
  resizer.resize(400, 400);
  resizer.dispose();
  resizer.resize(500, 500);
  advance(500);
  assert.deepEqual(calls, []);
  assert.equal(timers.size, 0);
});

test("invalid and hidden sizes never allocate zero-sized buffers", () => {
  const { resizer, calls, advance } = setup();
  for (const [width, height] of [
    [0, 230],
    [300, 0],
    [-1, 230],
    [NaN, 230],
    [300, Infinity],
  ]) {
    resizer.resize(width, height);
    advance(500);
  }
  assert.deepEqual(calls, []);
});

test("subpixel measurements round to one stable buffer size", () => {
  const { resizer, calls, advance } = setup();
  resizer.resize(300.1, 230.1);
  advance(500);
  assert.deepEqual(calls, []);
  resizer.resize(360.4, 250.6);
  advance(160);
  assert.deepEqual(calls, [["setSize", 360, 251], ["render"]]);
});

test("buffer resize and repaint happen synchronously, also without an animation loop", () => {
  const { resizer, calls, advance, timers } = setup();
  resizer.resize(450, 360);
  advance(160);
  assert.deepEqual(calls, [["setSize", 450, 360], ["render"]]);
  assert.equal(timers.size, 0);
});
