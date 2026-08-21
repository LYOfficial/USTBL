const fs = require("fs/promises");
const path = require("path");
const { execFileSync } = require("child_process");
const sharp = require("sharp");

const rootDir = path.resolve(__dirname, "..");
const iconPath = path.join(
  rootDir,
  "public",
  "images",
  "icons",
  "Logo_128x128.png"
);
const outputDir = path.join(rootDir, "src-tauri", "assets", "installer");

const colors = {
  navy: "#071b41",
  cobalt: "#104caa",
  azure: "#31a9e8",
  cyan: "#79ddff",
};

function headerBackground() {
  return Buffer.from(`
    <svg width="150" height="57" viewBox="0 0 150 57" xmlns="http://www.w3.org/2000/svg">
      <defs>
        <linearGradient id="background" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stop-color="${colors.navy}" />
          <stop offset="0.58" stop-color="${colors.cobalt}" />
          <stop offset="1" stop-color="${colors.azure}" />
        </linearGradient>
        <linearGradient id="beam" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stop-color="#ffffff" stop-opacity="0.23" />
          <stop offset="1" stop-color="#ffffff" stop-opacity="0" />
        </linearGradient>
      </defs>
      <rect width="150" height="57" fill="url(#background)" />
      <path d="M86 0H150V57H104Z" fill="url(#beam)" />
      <path d="M112 0L150 25V0ZM92 57L150 21V57Z" fill="#ffffff" fill-opacity="0.06" />
      <path d="M0 56.5H150" stroke="#ffffff" stroke-opacity="0.32" />
      <text x="56" y="27" fill="#ffffff" font-family="Segoe UI, Arial, sans-serif" font-size="17" font-weight="700" letter-spacing="1.4">USTBL</text>
      <text x="57" y="42" fill="#d7f4ff" font-family="Segoe UI, Arial, sans-serif" font-size="5.2" letter-spacing="0.35">MINECRAFT SERVER LAUNCHER</text>
    </svg>`);
}

function sidebarBackground() {
  return Buffer.from(`
    <svg width="164" height="314" viewBox="0 0 164 314" xmlns="http://www.w3.org/2000/svg">
      <defs>
        <linearGradient id="background" x1="0" y1="0" x2="0.9" y2="1">
          <stop offset="0" stop-color="${colors.navy}" />
          <stop offset="0.55" stop-color="#0d3982" />
          <stop offset="1" stop-color="${colors.cobalt}" />
        </linearGradient>
        <radialGradient id="glow" cx="50%" cy="45%" r="55%">
          <stop offset="0" stop-color="${colors.cyan}" stop-opacity="0.42" />
          <stop offset="1" stop-color="${colors.cyan}" stop-opacity="0" />
        </radialGradient>
        <linearGradient id="top" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stop-color="#7de6ff" stop-opacity="0.62" />
          <stop offset="1" stop-color="#2a8bd6" stop-opacity="0.25" />
        </linearGradient>
      </defs>
      <rect width="164" height="314" fill="url(#background)" />
      <rect width="164" height="314" fill="url(#glow)" />
      <g fill="none" stroke="#a6efff" stroke-opacity="0.10" stroke-width="0.65">
        <path d="M-26 204L82 142L190 204M-26 236L82 174L190 236M-26 268L82 206L190 268M-26 300L82 238L190 300" />
        <path d="M-2 126V314M25 110V314M52 94V314M79 79V314M106 94V314M133 110V314M160 126V314" />
      </g>
      <g opacity="0.43">
        <path d="M113 34l32-18 31 18-31 18z" fill="url(#top)" />
        <path d="M113 34v35l32 18V52z" fill="#0d4ca7" />
        <path d="M145 52v35l31-18V34z" fill="#0b3272" />
        <path d="M-17 235l39-22 38 22-38 22z" fill="url(#top)" />
        <path d="M-17 235v41l39 22v-41z" fill="#0c479f" />
        <path d="M22 257v41l38-22v-41z" fill="#0a2f6c" />
      </g>
      <path d="M0 0H164V1H0z" fill="#ffffff" fill-opacity="0.24" />
      <text x="18" y="54" fill="#ffffff" font-family="Segoe UI, Arial, sans-serif" font-size="26" font-weight="700" letter-spacing="1.4">USTBL</text>
      <text x="20" y="72" fill="#d7f4ff" font-family="Segoe UI, Arial, sans-serif" font-size="7.2" letter-spacing="0.6">MINECRAFT SERVER LAUNCHER</text>
      <path d="M20 87H144" stroke="#b5efff" stroke-opacity="0.55" />
      <text x="82" y="284" text-anchor="middle" fill="#d7f4ff" fill-opacity="0.88" font-family="Segoe UI, Arial, sans-serif" font-size="8.1" letter-spacing="0.9">BUILD · PLAY · CONNECT</text>
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
    "$graphics.Clear([System.Drawing.Color]::FromArgb(7, 27, 65))",
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
  const bitmapPath = path.join(outputDir, fileName);
  const pngPath = bitmapPath.replace(/\.bmp$/, ".png");
  await sharp(background)
    .png()
    .composite([
      {
        input: await sharp(iconPath)
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
    .flatten({ background: colors.navy })
    .png()
    .toFile(pngPath);
  convertPngToBitmap(pngPath, bitmapPath);
  await fs.unlink(pngPath);
}

async function main() {
  await fs.mkdir(outputDir, { recursive: true });

  await sharp(iconPath)
    .resize(256, 256, { fit: "contain", kernel: sharp.kernel.lanczos3 })
    .png()
    .toFile(
      path.join(
        rootDir,
        "src-tauri",
        "assets",
        "icons",
        "variants",
        "square.png"
      )
    );

  await compositeAsset({
    fileName: "nsis-header.bmp",
    background: headerBackground(),
    logo: { left: 7, top: 8, size: 42 },
  });
  await compositeAsset({
    fileName: "nsis-sidebar.bmp",
    background: sidebarBackground(),
    logo: { left: 28, top: 108, size: 108 },
  });
}

void main();
