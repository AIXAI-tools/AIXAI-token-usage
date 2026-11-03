import { defineConfig } from "vite";

// Tauri 開發模式固定用 1420 埠
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true },
});
