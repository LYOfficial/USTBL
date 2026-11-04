const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function read(file) {
  return fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
}

function load(file) {
  const module = { exports: {} };
  new Function(
    "exports",
    "module",
    ts.transpileModule(read(file), {
      compilerOptions: { module: ts.ModuleKind.CommonJS },
    }).outputText
  )(module.exports, module);
  return module.exports;
}

const { componentDisplayName } = load("src/utils/modpack-components.ts");

test("common component uids get fixed names and others show the uid", () => {
  assert.equal(componentDisplayName("net.minecraft"), "Minecraft");
  assert.equal(componentDisplayName("net.minecraftforge"), "Forge");
  assert.equal(componentDisplayName("net.neoforged"), "NeoForge");
  assert.equal(componentDisplayName("net.fabricmc.fabric-loader"), "Fabric");
  assert.equal(componentDisplayName("org.quiltmc.quilt-loader"), "Quilt");
  assert.equal(componentDisplayName("org.lwjgl3"), "LWJGL 3");
  assert.equal(
    componentDisplayName("com.example.custom"),
    "com.example.custom"
  );
});

test("the update preview lists component changes and keeps the loader line for plain packs", () => {
  const modal = read("src/components/modals/update-modpack-modal.tsx");
  assert.match(modal, /plan\.componentChanges\?\.length/);
  assert.match(modal, /UpdateModpackModal\.plan\.componentChanges\.title/);
  assert.match(modal, /<VersionChangeRow/);
  // The loader change is the fallback of the same block, not a second line.
  const block = modal.slice(
    modal.indexOf("plan.componentChanges?.length ?"),
    modal.indexOf("plan.restoreComponents &&")
  );
  assert.match(block, /plan\.loaderChange/);
});
