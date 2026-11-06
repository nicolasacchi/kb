# kb-term spike (throwaway)

A throwaway spike for the kb-term v2 design: live harness sessions shared between a local terminal and the web UI.
tmux holds each session, a control-mode tap mirrors raw bytes to every viewer, a headless xterm twin seeds late
joiners, and images travel as kitty Unicode placeholders drawn by a renderer from a content-addressed store.

This directory lives only on `spike/kb-term-*` branches. It is never merged into `main`.
All inputs are synthetic (see `gen/`); no real recordings, transcripts or images are used or committed.
CI is GitHub-hosted only: `.github/workflows/spike-kb-term.yml`.

## Layout

- `gen/` - synthetic generators (see `gen/README.md`).
- `package.json` - exact-pinned npm dependencies shared by the Node units (no lockfile).
- `<unit>/ci.sh` - the entry point of each unit: `strip`, `twin`, `renderer`, `phone`, `t4`.
  A unit with no `ci.sh` is skipped by CI. Each unit writes results to `<unit>/out/`, uploaded as artifact `<unit>-out`.

## Running a unit

CI does, from the repository root:

    cd spikes/kb-term && npm install --no-audit --no-fund    # twin, renderer, phone
    bash spikes/kb-term/<unit>/ci.sh

`ci.sh` is run from the repository root (not from the unit directory); resolve paths from `$(dirname "$0")`.
`strip` uses bun (zero-dependency TypeScript, no install). `renderer` and `phone` get Playwright browsers installed first.
