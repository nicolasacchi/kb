import { defineConfig } from "vite";
import { resolve } from "node:path";
import { execFileSync } from "node:child_process";
import react from "@vitejs/plugin-react";

// Stamp the git commit this bundle was built from into `__KB_BUILD_SHA__`.
// The daemon carries the same stamp (crates/kb-buildstamp/build.rs, injected
// into kb-server via `set_build_stamp` — PF-B1)
// and reports it on /api/identity; the shell compares the two and warns on
// drift — a fresh bundle served by a stale daemon is the white-screen footgun
// when the HTTP contract has changed. Absent git → "unknown" (comparison
// silently no-ops). `execFileSync` (no shell) keeps this injection-free.
function gitSha(): string {
  // The Docker build excludes .git (so `git` can't run), so the image build
  // passes the commit via KB_GIT_SHA — the same value the daemon binary's
  // KB_BUILD_SHA carries, keeping the drift comparison like-for-like.
  const injected = process.env.KB_GIT_SHA?.trim();
  if (injected) return injected;
  try {
    const git = (args: string[]) =>
      execFileSync("git", args, { encoding: "utf8" }).trim();
    const sha = git(["rev-parse", "--short=12", "HEAD"]);
    const dirty = git(["status", "--porcelain"]) ? "-dirty" : "";
    return sha + dirty;
  } catch {
    return "unknown";
  }
}

// kb SPA build. Output goes into web/dist; the daemon serves it via
// ServeDir at "/". `base: "/"` keeps asset paths absolute so artifact
// subdomains (`<id>.artifacts.localhost`) don't break when the SPA
// reloads under a different host. Dev port aligns with the daemon
// default (4737) +1 so `npm run dev` and `kb daemon` can run side by
// side without colliding.
//
// Two build entries:
//   - main      → dist/assets/main-<hash>.js  (the React SPA shell)
//   - annotate  → dist/annotate.js            (the in-iframe annotator;
//                                              no hash so the daemon's
//                                              `/_kb/annotate.js` alias
//                                              resolves via direct name)
//   - sketch    → dist/sketch.html            (SL4 — the slate board's
//                                              mermaid frame; sandboxed,
//                                              opaque origin, never
//                                              imported by the SPA)
//
// The annotator must stay tiny (CI fails the build if `dist/annotate.js`
// exceeds 12 KiB — see the bundle-size guard in .github/workflows/ci.yml).
// Vanilla DOM only — never import React or any other SPA code.
//
// There is NO `output.format: "iife"` for the annotate entry: it is emitted
// by the same build as `main`/`sketch`, as a classic script, and stays
// collision-safe only because `src/scripts/annotate.ts` wraps its own body
// in an IIFE. The guard is tests/e2e/annotator-bundle-isolation.spec.ts
// (e2e job only) — keep the source IIFE.
export default defineConfig({
  plugins: [react()],
  base: "/",
  // CodeMirror 6 breaks subtly if two copies of @codemirror/state (or
  // @lezer/common) load — extensions/tags from one instance are unrecognised
  // by the other. Force a single copy. (The editor module is in the `main`
  // chunk only; never imported from `annotate.ts`, whose 12 KiB CI guard would
  // otherwise trip.)
  resolve: {
    dedupe: ["@codemirror/state", "@lezer/common"],
  },
  define: {
    __KB_BUILD_SHA__: JSON.stringify(gitSha()),
  },
  // SW2 — the kb-sse SharedWorker (src/workers/sse.worker.ts) imports
  // sse/core.ts; ES format keeps that worker bundle a module if Rollup
  // ever splits a chunk out of it. The worker is its own bundler pass, so
  // the annotate entryFileNames special-case below never sees it.
  worker: {
    format: "es",
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    // PF-S1 (operator ruling): prod bundles ship no maps — nobody debugged
    // prod from maps, and they dominated dist/ size + build IO on the
    // disk-bound host.
    sourcemap: false,
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        annotate: resolve(__dirname, "src/scripts/annotate.ts"),
        // SL4 — the slate board's sandboxed drawing frame. Its own HTML
        // entry (dist/sketch.html) because it is loaded by URL into an
        // `<iframe sandbox="allow-scripts">` with no allow-same-origin, and
        // because it bundles mermaid: keeping it OUT of the `main` graph is
        // what stops the SPA shell paying for a diagram renderer it never
        // calls. Rollup emits it as its own chunk under assets/.
        sketch: resolve(__dirname, "sketch.html"),
      },
      output: {
        entryFileNames: (chunkInfo) =>
          chunkInfo.name === "annotate"
            ? "annotate.js"
            : "assets/[name]-[hash].js",
        chunkFileNames: "assets/[name]-[hash].js",
        assetFileNames: "assets/[name]-[hash].[ext]",
        // Vendor-split node_modules into long-cache chunks so a heavy library
        // (CodeMirror, the markdown stack) loads once and is cached across
        // deploys, and the route-lazy chunks above don't each re-bundle it.
        // These are rolldown `codeSplitting` groups (Vite 8's replacement for
        // the deprecated function-form `manualChunks`); higher `priority`
        // wins when a module matches two groups, which reproduces the old
        // first-`if`-wins order. Runs only over the `main` entry's graph; the
        // `annotate` entry and the SharedWorker import none of this, so the
        // 12 KiB annotate.js CI guard is unaffected.
        //
        // SL4 — mermaid is deliberately in NO group. Naming a bucket for it
        // was tried and reverted: a forced chunk makes the bundler hoist
        // Vite's shared `__vite__preload` helper into whichever group chunk
        // it visits first, and with a `mermaid` bucket that was the 3.1 MB
        // mermaid chunk — every SPA route chunk then carried a hard
        // `import "./mermaid-*.js"`. Left to the default algorithm mermaid is
        // reachable ONLY from `src/sketch/main.ts` (the second HTML entry).
        // That used to be a comment; `scripts/check-main-chunk.mjs` (run by
        // `npm run build`) now fails the build if a large sketch-graph chunk
        // appears in the shell's static import closure.
        codeSplitting: {
          groups: [
            {
              name: "codemirror",
              test: /node_modules[\\/](?:.*[\\/])?@(?:codemirror|lezer)[\\/]/,
              priority: 40,
            },
            {
              name: "markdown",
              test: /node_modules[\\/](?:.*[\\/])?(?:react-markdown|remark-[^\\/]*|micromark[^\\/]*|mdast[^\\/]*|unist[^\\/]*|hast[^\\/]*)[\\/]/,
              priority: 30,
            },
            {
              name: "tanstack",
              test: /node_modules[\\/]@tanstack[\\/]/,
              priority: 20,
            },
            {
              name: "react-vendor",
              test: /node_modules[\\/](?:react-router[^\\/]*|react-dom|react|scheduler)[\\/]/,
              priority: 10,
            },
          ],
        },
      },
    },
  },
  server: {
    port: 4738,
    strictPort: true,
    proxy: {
      "/api": "http://127.0.0.1:4737",
      "/a": "http://127.0.0.1:4737",
    },
  },
});
