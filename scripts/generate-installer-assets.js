const fs = require("fs/promises");
const path = require("path");
const { execFileSync } = require("child_process");
const sharp = require("sharp");

const rootDir = path.resolve(__dirname, "..");
const brandingDir = path.join(rootDir, "src-tauri", "assets", "branding");
const transparentLogoPath = path.join(brandingDir, "ustbl-logo.png");
const roundedLogoPath = path.join(brandingDir, "ustbl-logo-rounded.png");
const iconOutputDir = path.join(rootDir, "src-tauri", "assets", "icons");
const installerOutputDir = path.join(
  rootDir,
  "src-tauri",
  "assets",
  "installer"
);
const publicLogoPath = path.join(
  rootDir,
  "public",
  "images",
  "icons",
  "Logo_128x128.png"
);

const colors = {
  midnight: "#06172f",
  navy: "#092b55",
  blue: "#176cb0",
  sky: "#55b3e9",
  cream: "#f2e5b4",
};

function headerBackground() {
  return Buffer.from(`
    <svg width="150" height="57" viewBox="0 0 150 57" xmlns="http://www.w3.org/2000/svg">
      <defs>
        <linearGradient id="background" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stop-color="${colors.midnight}" />
          <stop offset="0.62" stop-color="${colors.navy}" />
          <stop offset="1" stop-color="${colors.blue}" />
        </linearGradient>
        <radialGradient id="glow" cx="12%" cy="50%" r="75%">
          <stop offset="0" stop-color="${colors.sky}" stop-opacity="0.24" />
          <stop offset="1" stop-color="${colors.sky}" stop-opacity="0" />
        </radialGradient>
      </defs>
      <rect width="150" height="57" fill="url(#background)" />
      <rect width="150" height="57" fill="url(#glow)" />
      <path d="M103 -8C118 7 130 22 154 25" fill="none" stroke="#ffffff" stroke-opacity="0.08" />
      <path d="M96 64C115 43 130 38 156 35" fill="none" stroke="#ffffff" stroke-opacity="0.06" />
      <rect y="55" width="150" height="2" fill="${colors.cream}" fill-opacity="0.9" />
      <text x="57" y="27" fill="#ffffff" font-family="Segoe UI, Arial, sans-serif" font-size="17" font-weight="700" letter-spacing="1.5">USTBL</text>
      <text x="58" y="42" fill="#d7edfb" font-family="Segoe UI, Arial, sans-serif" font-size="6.2" letter-spacing="0.65">MINECRAFT LAUNCHER</text>
    </svg>`);
}

function sidebarBackground() {
  return Buffer.from(`
    <svg width="164" height="314" viewBox="0 0 164 314" xmlns="http://www.w3.org/2000/svg">
      <defs>
        <linearGradient id="background" x1="0" y1="0" x2="0.9" y2="1">
          <stop offset="0" stop-color="${colors.midnight}" />
          <stop offset="0.58" stop-color="${colors.navy}" />
          <stop offset="1" stop-color="#0d4f87" />
        </linearGradient>
        <radialGradient id="logoGlow" cx="50%" cy="32%" r="45%">
          <stop offset="0" stop-color="${colors.sky}" stop-opacity="0.3" />
          <stop offset="1" stop-color="${colors.sky}" stop-opacity="0" />
        </radialGradient>
        <linearGradient id="sweep" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stop-color="#ffffff" stop-opacity="0.08" />
          <stop offset="1" stop-color="#ffffff" stop-opacity="0" />
        </linearGradient>
      </defs>
      <rect width="164" height="314" fill="url(#background)" />
      <rect width="164" height="314" fill="url(#logoGlow)" />
      <path d="M-20 225C35 190 84 210 184 146V314H-20Z" fill="url(#sweep)" />
      <circle cx="82" cy="101" r="63" fill="none" stroke="#ffffff" stroke-opacity="0.07" />
      <circle cx="82" cy="101" r="54" fill="none" stroke="#ffffff" stroke-opacity="0.05" />
      <path d="M35 210H129" stroke="${colors.cream}" stroke-width="1.5" stroke-opacity="0.9" />
      <text x="82" y="244" text-anchor="middle" fill="#ffffff" font-family="Segoe UI, Arial, sans-serif" font-size="25" font-weight="700" letter-spacing="2.4">USTBL</text>
      <text x="82" y="263" text-anchor="middle" fill="#d7edfb" font-family="Segoe UI, Arial, sans-serif" font-size="7.2" letter-spacing="0.85">MINECRAFT LAUNCHER</text>
      <text x="82" y="291" text-anchor="middle" fill="${colors.cream}" fill-opacity="0.9" font-family="Segoe UI, Arial, sans-serif" font-size="6.4" letter-spacing="0.8">USTB · BUILD · PLAY</text>
    </svg>`);
}

function convertPngToBitmap(sourcePath, destinationPath) {
  if (process.platform !== "win32") {
    throw new Error("NSIS installer artwork must be generated on Windows.");
  }

  const quoteForPowerShell = (value) => `'${value.replaceAll("'", "''")}'`;
  const command = [
    "Add-Type -AssemblyName System.Drawing",
    `$source = [System.Drawing.Image]::FromFile(${quoteForPowerShell(sourcePath)})`,
    "$bitmap = New-Object System.Drawing.Bitmap $source.Width, $source.Height, ([System.Drawing.Imaging.PixelFormat]::Format24bppRgb)",
    "$graphics = [System.Drawing.Graphics]::FromImage($bitmap)",
    `$graphics.Clear([System.Drawing.ColorTranslator]::FromHtml('${colors.midnight}'))`,
    "$graphics.DrawImage($source, 0, 0, $source.Width, $source.Height)",
    "$graphics.Dispose()",
    `$bitmap.Save(${quoteForPowerShell(destinationPath)}, [System.Drawing.Imaging.ImageFormat]::Bmp)`,
    "$bitmap.Dispose()",
    "$source.Dispose()",
  ].join("; ");
  execFileSync(
    "powershell.exe",
    ["-NoProfile", "-NonInteractive", "-Command", command],
    { stdio: "inherit" }
  );
}

async function compositeAsset({ fileName, background, logo }) {
  const bitmapPath = path.join(installerOutputDir, fileName);
  const pngPath = bitmapPath.replace(/\.bmp$/, ".png");
  await sharp(background)
    .png()
    .composite([
      {
        input: await sharp(transparentLogoPath)
          .resize(logo.size, logo.size, {
            fit: "contain",
            kernel: sharp.kernel.lanczos3,
          })
          .png()
          .toBuffer(),
        left: logo.left,
        top: logo.top,
      },
    ])
    .flatten({ background: colors.midnight })
    .png()
    .toFile(pngPath);
  convertPngToBitmap(pngPath, bitmapPath);
  await fs.unlink(pngPath);
}

async function generateWindowsIcon() {
  const generatedIconDir = path.join(
    rootDir,
    "src-tauri",
    "assets",
    ".generated-icons"
  );
  await fs.rm(generatedIconDir, { recursive: true, force: true });
  const tauriCli = require.resolve("@tauri-apps/cli/tauri.js");
  execFileSync(
    process.execPath,
    [tauriCli, "icon", roundedLogoPath, "--output", generatedIconDir],
    { cwd: rootDir, stdio: "inherit" }
  );
  await fs.copyFile(
    path.join(generatedIconDir, "icon.ico"),
    path.join(iconOutputDir, "icon.ico")
  );
  await fs.rm(generatedIconDir, { recursive: true, force: true });
}

async function main() {
  await fs.mkdir(installerOutputDir, { recursive: true });
  await fs.mkdir(path.dirname(publicLogoPath), { recursive: true });
  await fs.mkdir(path.join(iconOutputDir, "variants"), { recursive: true });

  await Promise.all([
    sharp(roundedLogoPath)
      .resize(128, 128, { fit: "contain", kernel: sharp.kernel.lanczos3 })
      .png()
      .toFile(publicLogoPath),
    sharp(roundedLogoPath)
      .resize(256, 256, { fit: "contain", kernel: sharp.kernel.lanczos3 })
      .png()
      .toFile(path.join(iconOutputDir, "variants", "square.png")),
  ]);

  await generateWindowsIcon();
  await compositeAsset({
    fileName: "nsis-header.bmp",
    background: headerBackground(),
    logo: { left: 8, top: 8, size: 41 },
  });
  await compositeAsset({
    fileName: "nsis-sidebar.bmp",
    background: sidebarBackground(),
    logo: { left: 31, top: 50, size: 102 },
  });
}

void main();
