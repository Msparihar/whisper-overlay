import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";
import { fileURLToPath } from "node:url";

const harnessDir = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  root: harnessDir,
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@tauri-apps/api/event": path.resolve(harnessDir, "src/tauri-event-mock.ts"),
      "@": path.resolve(harnessDir, "../../src"),
      clsx: path.resolve(harnessDir, "node_modules/clsx"),
      react: path.resolve(harnessDir, "node_modules/react"),
      "react-dom": path.resolve(harnessDir, "node_modules/react-dom"),
      "tailwind-merge": path.resolve(harnessDir, "node_modules/tailwind-merge"),
    },
  },
  server: {
    host: "127.0.0.1",
    port: 1421,
    strictPort: true,
    fs: {
      allow: [path.resolve(harnessDir, "../..")],
    },
  },
});
