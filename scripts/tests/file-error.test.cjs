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
      { compilerOptions: { module: ts.ModuleKind.CommonJS } }
    ).outputText
  )(module.exports, module, requireModule);
  return module.exports;
}

const fileError = load("src/utils/file-error.ts");
const requireFileError = (name) =>
  name === "@/utils/file-error" ? fileError : {};
const campus = load("src/utils/campus-install-task.ts", requireFileError);
const update = load("src/utils/modpack-update-task.ts", (name) =>
  name === "@/utils/file-error"
    ? fileError
    : name.endsWith("modpack-update-choices")
      ? { failedUpdatePlans: new Map(), rememberedChoices: new Map() }
      : campus
);

const locales = ["zh-Hans", "zh-Hant", "en", "fr", "ja"];
const messages = Object.fromEntries(
  locales.map((lang) => [
    lang,
    JSON.parse(
      fs.readFileSync(
        path.join(__dirname, `../../src/locales/${lang}.json`),
        "utf8"
      )
    ),
  ])
);

const translator =
  (lang) =>
  (key, options = {}) => {
    const value = key
      .split(".")
      .reduce((node, part) => node?.[part], messages[lang]);
    if (typeof value !== "string") return options.defaultValue ?? key;
    return value.replace(/\{\{(\w+)\}\}/g, (_, name) => options[name] ?? "");
  };

const wire = (code, file, detail) =>
  `${code}:${JSON.stringify({ file, detail })}`;

test("a file error text is read back with its code, file and detail", () => {
  const text = wire(
    "FILE_BLOCKED_BY_ANTIVIRUS",
    'mods\\a "b" {c}.jar',
    "拒绝访问。 (os error 225)"
  );
  assert.deepEqual(fileError.parseFileError(text), {
    code: "FILE_BLOCKED_BY_ANTIVIRUS",
    file: 'mods\\a "b" {c}.jar',
    detail: "拒绝访问。 (os error 225)",
  });
  assert.equal(
    fileError.parseFileError(`${text}; rollback failed: x }`)?.file,
    'mods\\a "b" {c}.jar'
  );
});

test("plain strings and unknown codes are not file errors", () => {
  for (const text of [
    "",
    "NETWORK_ERROR",
    "FILE_OPERATION_ERROR",
    "failed (os error 5)",
    'SOMETHING:{"file":"a"}',
    'FILE_MISSING:{"file":',
    "FILE_MISSING:{}",
  ]) {
    assert.equal(fileError.parseFileError(text), null, text);
  }
  assert.equal(fileError.parseFileError(undefined), null);
});

test("every category gives its own sentence in every locale", () => {
  const cases = {
    FILE_BLOCKED_BY_ANTIVIRUS: "mods\\a.jar",
    FILE_MISSING: "mods\\a.jar",
    FILE_INACCESSIBLE: "mods\\a.jar",
    DISK_FULL: "mods\\a.jar",
    FILE_IO_FAILED: "mods\\a.jar",
  };
  for (const lang of locales) {
    const seen = new Set();
    for (const [code, file] of Object.entries(cases)) {
      const text = fileError.localizeFileError(
        translator(lang),
        wire(code, file, "oops")
      );
      assert.ok(text.includes(file), `${lang} ${code} names the file`);
      assert.ok(!text.includes("{{"), `${lang} ${code} has no placeholder`);
      assert.equal(text.includes("oops"), code === "FILE_IO_FAILED");
      seen.add(text);
    }
    assert.equal(seen.size, 5, lang);
  }
});

test("the zh-Hans copy is the agreed wording", () => {
  const t = translator("zh-Hans");
  const say = (code) =>
    fileError.localizeFileError(t, wire(code, "mods\\a.jar", "拒绝访问"));
  assert.equal(
    say("FILE_BLOCKED_BY_ANTIVIRUS"),
    "文件 mods\\a.jar 被杀毒软件拦截。请在 Windows 安全中心允许该文件，或把游戏目录加入排除项后重试。"
  );
  assert.equal(
    say("FILE_MISSING"),
    "找不到文件 mods\\a.jar，可能已被删除、移动或被杀毒软件隔离。请检查后重试。"
  );
  assert.equal(
    say("FILE_INACCESSIBLE"),
    "无法访问文件 mods\\a.jar，可能正被游戏或其他程序占用，或没有权限。请关闭相关程序后重试。"
  );
  assert.equal(
    say("DISK_FULL"),
    "磁盘空间不足，无法写入 mods\\a.jar。请清理磁盘后重试。"
  );
  assert.equal(
    say("FILE_IO_FAILED"),
    "读写文件 mods\\a.jar 失败（拒绝访问）。"
  );
});

test("install and update localisers show file errors and keep old strings working", () => {
  const t = translator("zh-Hans");
  const text = wire("FILE_MISSING", "mods\\a.jar", "not found");
  const expected = fileError.localizeFileError(t, text);
  assert.equal(campus.localizeCampusInstallError(t, text), expected);
  assert.equal(update.localizeModpackUpdateError(t, text), expected);

  // A network word inside the detail must not turn it into a network error.
  const noisy = wire(
    "FILE_IO_FAILED",
    "a.jar",
    "connection reset (os error 1)"
  );
  assert.match(campus.localizeCampusInstallError(t, noisy), /a\.jar/);
  assert.match(update.localizeModpackUpdateError(t, noisy), /a\.jar/);

  // Old unstructured strings still reach the previous fallbacks.
  assert.equal(
    campus.localizeCampusInstallError(t, "connection timed out"),
    t("Services.instance.installCampusModpack.error.description.NETWORK_ERROR")
  );
  assert.equal(
    campus.localizeCampusInstallError(t, "failed (os error 5)"),
    t(
      "Services.instance.installCampusModpack.error.description.FILE_OPERATION_ERROR"
    )
  );
  assert.equal(
    update.localizeModpackUpdateError(t, "failed (os error 5)"),
    t(
      "Services.instance.applyModpackUpdate.error.description.FILE_OPERATION_ERROR"
    )
  );
});

test("the response handler turns a file error into the localised details", async () => {
  const t = translator("zh-Hans");
  const { responseHandler } = load("src/utils/response.ts", (name) =>
    name === "i18next"
      ? { t }
      : name === "@/utils/env"
        ? { isDev: false }
        : name === "@/utils/file-error"
          ? fileError
          : name === "@/utils/logging"
            ? { logger: { error: () => {} } }
            : {}
  );
  const text = wire("DISK_FULL", "mods\\a.jar", "full");
  const descriptor = {
    value: async () => {
      throw text;
    },
  };
  responseHandler("instance")({}, "applyModpackUpdate", descriptor);
  const response = await descriptor.value();
  assert.equal(response.status, "error");
  assert.equal(response.raw_error, text);
  assert.equal(response.details, fileError.localizeFileError(t, text));
});
