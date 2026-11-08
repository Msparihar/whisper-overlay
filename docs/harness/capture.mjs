import { spawn, spawnSync } from "node:child_process";
import { copyFile, mkdtemp, readdir, rm, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const harnessDir = path.dirname(fileURLToPath(import.meta.url));
const docsDir = path.resolve(harnessDir, "..");
const frameDir = await mkdtemp(path.join(os.tmpdir(), "whisper-overlay-frames-"));
const pageUrl = "http://127.0.0.1:1421";
const viteBin = path.join(harnessDir, "node_modules", ".bin", "vite");
const server = spawn(viteBin, ["--config", "vite.config.ts"], {
  cwd: harnessDir,
  stdio: "ignore",
});

async function waitForVite() {
  for (let attempt = 0; attempt < 60; attempt += 1) {
    try {
      const response = await fetch(pageUrl);
      if (response.ok) return;
    } catch {
      // Vite is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("Vite did not start on port 1421");
}

try {
  await waitForVite();

  const browser = await chromium.launch({
    headless: true,
    args: ["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream"],
  });
  const context = await browser.newContext({
    viewport: { width: 350, height: 56 },
    deviceScaleFactor: 2,
    permissions: ["microphone"],
  });
  const page = await context.newPage();
  await page.goto(pageUrl, { waitUntil: "networkidle" });
  const renderError = await page.locator("vite-error-overlay").count();
  if (renderError > 0) {
    throw new Error(await page.locator("vite-error-overlay").innerText());
  }

  for (let frame = 0; frame < 52; frame += 1) {
    const framePath = path.join(frameDir, `frame-${String(frame).padStart(3, "0")}.png`);
    await page.screenshot({ path: framePath, omitBackground: true });
    if (frame === 18) {
      await copyFile(framePath, path.join(docsDir, "overlay-demo.png"));
    }
    await page.waitForTimeout(80);
  }

  await browser.close();

  const gifPath = path.join(docsDir, "overlay-demo.gif");
  const ffmpeg = spawnSync(
    "ffmpeg",
    [
      "-y",
      "-framerate", "12",
      "-i", path.join(frameDir, "frame-%03d.png"),
      "-filter_complex",
      "fps=12,split[a][b];[a]palettegen=max_colors=64:reserve_transparent=1[p];[b][p]paletteuse=dither=bayer:bayer_scale=3:alpha_threshold=128",
      gifPath,
    ],
    { stdio: "inherit" },
  );
  if (ffmpeg.status !== 0) {
    throw new Error("ffmpeg failed to create the demo GIF");
  }

  const gifSize = (await stat(gifPath)).size;
  if (gifSize >= 3 * 1024 * 1024) {
    throw new Error(`Demo GIF is ${gifSize} bytes; it must stay below 3 MB`);
  }

  const frames = await readdir(frameDir);
  console.log(`Captured ${frames.length} frames; GIF size ${gifSize} bytes.`);
} finally {
  server.kill("SIGTERM");
  await rm(frameDir, { recursive: true, force: true });
}
