const fs = require("fs");
const path = require("path");
const { execSync } = require("child_process");

const newVersion = process.argv[3];

if (!newVersion) {
  console.error("Usage: npm run version bump <new-version>");
  process.exit(1);
}

// Update package.json
const packageJsonPath = path.join(__dirname, "../../package.json");
const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, "utf8"));
packageJson.version = newVersion;
fs.writeFileSync(packageJsonPath, JSON.stringify(packageJson, null, 2) + "\n");

// Update tauri.conf.json
const tauriConfigPath = path.join(__dirname, "../../src-tauri/tauri.conf.json");
const tauriConfig = JSON.parse(fs.readFileSync(tauriConfigPath, "utf8"));
tauriConfig.version = newVersion;
fs.writeFileSync(tauriConfigPath, JSON.stringify(tauriConfig, null, 2) + "\n");

// Update Cargo.toml
const cargoTomlPath = path.join(__dirname, "../../src-tauri/Cargo.toml");
let cargoToml = fs.readFileSync(cargoTomlPath, "utf8");
cargoToml = cargoToml.replace(
  /version\s*=\s*"[^"]+"/,
  `version = "${newVersion}"`
);
fs.writeFileSync(cargoTomlPath, cargoToml);

// Update Cargo.lock, which records the workspace package version. Leaving it behind makes
// `cargo test --locked` / `cargo build --locked` fail with "cannot update the lock file".
// Cargo.lock is edited in place instead of running `cargo update`, so this works offline.
const cargoLockPath = path.join(__dirname, "../../src-tauri/Cargo.lock");
const cargoLock = fs.readFileSync(cargoLockPath, "utf8");
const cargoLockPattern =
  /(\[\[package\]\]\r?\nname = "USTBL"\r?\nversion = ")[^"]+(")/;
if (!cargoLockPattern.test(cargoLock)) {
  console.error("❌ Could not find the USTBL package entry in Cargo.lock");
  process.exit(1);
}
const updatedCargoLock = cargoLock.replace(
  cargoLockPattern,
  `$1${newVersion}$2`
);
if (updatedCargoLock !== cargoLock) {
  fs.writeFileSync(cargoLockPath, updatedCargoLock);
}

console.log(`✅ Updated all version numbers to ${newVersion}`);

// Sync package-lock.json with package.json
console.log("\n🔄 Syncing package-lock.json with package.json...");
try {
  execSync(
    "npm install --package-lock-only --no-audit --no-fund --ignore-scripts",
    {
      stdio: "inherit",
      cwd: path.join(__dirname, "../../"),
    }
  );
  console.log("✅ package-lock.json synced successfully!");
} catch (error) {
  console.error("❌ Failed to sync package-lock.json:", error.message);
  process.exit(1);
}
