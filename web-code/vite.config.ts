import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// kb-code SPA build (W4.1). Output goes into web-code/dist; the
// kb-code-server daemon serves it via its own hand-rolled fallback
// (`crates/kb-code-server/src/spa.rs`, mirroring kb's own
// `crates/kb-server/src/routes/spa.rs`). `base: "/"` keeps asset paths
// absolute — kb-code has no artifact-subdomain equivalent (one daemon,
// one origin), so this is simpler than kb's own `web/vite.config.ts`,
// which carries that concern.
//
// Dev port 4748 pairs with the daemon's own default 4747
// (`kb-code.toml`'s `[server] addr`, `ServerSection::DEFAULT_ADDR`) —
// same "SPA dev port = daemon port + 1" convention kb's own `web/`
// uses (4738 ↔ 4737).
export default defineConfig({
  plugins: [react()],
  base: "/",
  // CodeMirror 6 breaks subtly if two copies of @codemirror/state load —
  // same guard kb's own web/vite.config.ts carries.
  resolve: {
    dedupe: ["@codemirror/state"],
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    // PF-S1 (operator ruling): prod bundles ship no maps — nobody debugged
    // prod from maps, and they dominated dist/ size + build IO on the
    // disk-bound host.
    sourcemap: false,
    rolldownOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes("node_modules")) return;
          if (id.includes("@codemirror") || id.includes("@lezer")) return "codemirror";
          if (id.includes("@tanstack")) return "tanstack";
          if (
            id.includes("react-router") ||
            id.includes("/react-dom/") ||
            id.includes("/react/") ||
            id.includes("/scheduler/")
          )
            return "react-vendor";
        },
      },
    },
  },
  server: {
    port: 4748,
    strictPort: true,
    proxy: {
      "/api": "http://127.0.0.1:4747",
    },
  },
});
