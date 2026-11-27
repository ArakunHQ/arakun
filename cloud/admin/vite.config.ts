// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { cloudflare } from "@cloudflare/vite-plugin";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, searchForWorkspaceRoot } from "vite";

export default defineConfig({
  plugins: [
    // Before react(): it writes src/routeTree.gen.ts from src/routes and
    // splits each route into its own chunk.
    tanstackRouter({ target: "react", autoCodeSplitting: true, routeFileIgnorePattern: "\\.test\\." }),
    react(),
    cloudflare(),
  ],
  // The Geist files live once, in the repository's phone/fonts.
  server: { fs: { allow: [searchForWorkspaceRoot(process.cwd()), "../../phone/fonts"] } },
});
