import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// The Worker and the desktop app serve dist/ as it is, so the file names
// carry no hash and the page can be checked in and diffed.
export default defineConfig({
  root: ".",
  base: "./",
  publicDir: "public",
  plugins: [react()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: ["safari16.4", "chrome111"],
    modulePreload: { polyfill: false },
    rollupOptions: {
      input: "index.html",
      output: {
        entryFileNames: "assets/app.js",
        chunkFileNames: "assets/[name].js",
        assetFileNames: "assets/[name][extname]",
      },
    },
  },
  test: {
    environment: "jsdom",
    setupFiles: ["src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
