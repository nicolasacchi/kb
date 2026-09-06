# spike-findings

A log of what the six de-risking spikes confirmed, surprised, or refuted.
Each section corresponds to one spike. The architecture decisions in
`docs/research/*.html` are **never edited** — this file is the delta.

The order matches the research-recommended spike order (`docs/research/spikes.html`).

| spike       | status       | retired at | research source                          |
|-------------|--------------|------------|------------------------------------------|
| walker      | worked       | 2026-05-11 | `docs/research/07-html-effectiveness.html` |
| fastembed   | partial      | 2026-05-11 | `docs/research/02-fastembed.html`         |
| sse         | worked       | 2026-05-11 | `docs/research/04-sse-axum.html`          |
| iframe      | worked       | 2026-05-11 | `docs/research/06-iframe.html`            |
| ratatui     | worked       | 2026-05-11 | `docs/research/05-ratatui.html`           |
| lance       | worked       | 2026-05-11 | `docs/research/01-lancedb.html`           |

---

<!-- Each spike's findings appended below by `just findings <name>`. -->

---

## spike-walker · 2026-05-11

**Status:** worked
**Effort:** 1h estimated, ~1h actual
**Crate hash:** spike branch `spike/walker` (initial scaffold at `7a4c3e0`)
**Research source:** `docs/research/07-html-effectiveness.html`

### Goal
Parse a directory of HTML files with `scraper`, extract a 6-field summary per file, time per file. Validate that field extraction matches topic 07's listing and that real artifacts parse without panicking.

### Worked as expected
- `scraper` (html5ever-backed) parses every artifact in `corpus/canon/` (8 files including `pm/` subdir) without panic.
- Six-field summary `{title, h1, p, svg_count, has_details, has_script}` extracts cleanly for every file.
- Per-file parse latency is **well below** the topic-07 estimate of "<5 ms/file". On this machine: **mean 159 μs, p50 155 μs, p95 294 μs** across the 104-file synthetic corpus.
- Whitespace normalization (collapsing newlines/tabs in `<title>` content via `split_whitespace().join(" ")`) yields clean values without any surprises.
- The malformed-input panic-fuzz battery (empty, unclosed tags, malformed comments, nested `<svg>`, script-with-fake-title-inside) **never panics** — `scraper` swallows them all gracefully.

### Surprised
- The "first `<p>`" extraction returns `None` for `fullscreen-viz.html` because the artifact is SVG-driven and has no body-level `<p>` outside the SVG. A real indexer that wants `body excerpt` should fall back to a different selector (e.g., first text node under `<body>` excluding `<script>/<style>`) for SVG-heavy artifacts.
- Topic 07's field-distribution sampling ("11/20 inline SVG, 6/20 `<details>`, 14/20 `<script>`") was measured on Thariq's 20 reference artifacts. The 5 canon artifacts (+ 4 postmortem pages = 8 total HTML files) have a noticeably different distribution: **1/8 SVG, 1/8 `<details>`, 8/8 `<script>`**. The canon set is not statistically representative of "all artifacts" — production indexer benchmarks need a larger corpus before any distribution claims.
- All 8 canon files contain `<script>` (kb-research's own design system embeds a theme-toggle script in every artifact). The walker correctly flags this; downstream code should not treat "no `<script>`" as a common case.

### Failed / blocked
- N/A.

### Measurements
| metric                           | expected (research) | observed (this machine) | verdict |
|----------------------------------|---------------------|-------------------------|---------|
| Parse μs/file p95                | < 5000 μs           | 294 μs (104 files)      | ✅ ~17× faster than budget |
| Parse μs/file mean               | < 5000 μs           | 159 μs (104 files)      | ✅ ~31× faster |
| Total wallclock 100 files        | < 1000 ms           | 17.55 ms (binary-only)  | ✅ ~57× faster |
| All 6 fields extracted per file  | 6/6 every file      | 6/6 every file          | ✅ |
| Panics on canon + synthetic      | none                | none across 104+8 files | ✅ |
| Panics on malformed-input fuzz   | none                | none across 8 inputs    | ✅ |

### Architecture decision updates
- `07-html-effectiveness.html` §D — **CONFIRMED.** The 6-field extraction shape from the research (title / h1 / p / svg_count / has_details / has_script) is the right starting set. Parsing cost is small enough that we can add more fields freely without budget worry. Action: production `kb-core::parser` carries these six fields forward as a baseline; additional facets (`has_form`, `has_canvas`, `has_animation`, `has_math`, `kb_category`, `prompt_size_bytes`, `js_loc`, `css_loc`) listed in `01-lancedb.html` schema can be added incrementally without re-evaluating the parser.
- `07-html-effectiveness.html` §C distribution claims — **REVISED in scope.** The "11/20 SVG, 6/20 details, 14/20 script" numbers are corpus-specific. Production code should not make any density-based assumptions; index every facet as a bool/count and let users filter at query time. Action: when production indexer ships, file a follow-up to sample distribution against the user's full curated corpus (≥50 artifacts) and update the research with corrected numbers.
- **NEW finding:** "first `<p>`" is unreliable as a "body excerpt" proxy for SVG-heavy artifacts. Production extractor should also store a `body_text_excerpt` field derived from a depth-first walk that skips `<script>/<style>` and concatenates the first ~400 chars of visible text. Action: open an issue against `kb-core` to specify this for the production parser.

### Manual procedure log
```
[1/4] Building release binary...
[2/4] Run against corpus/canon (the 5 frozen artifacts + pm/ subdir)...
------------------------------------------------------------
path,title,h1_present,p_chars,svg,details,script,parse_us
cost-of-abstraction.html,The Cost of Abstraction — a four-page essay,Y,18,0,N,Y,437
fullscreen-viz.html,Visualizing the Borrow Checker,Y,0,1,N,Y,170
kitchen-sink.html,"Everything HTML Can Do, in One Page",Y,164,0,Y,Y,309
multi-page.html,A Field Guide to Rust Errors,Y,118,0,N,Y,241
pm/00-summary.html,INC-0315 · Summary,Y,10,0,N,Y,68
pm/01-timeline.html,INC-0315 · Timeline,Y,18,0,N,Y,82
pm/02-cause.html,INC-0315 · Root Cause,Y,20,0,N,Y,84
pm/03-actions.html,INC-0315 · Action Items,Y,22,0,N,Y,118

spike-walker summary:
  corpus:           ~/project/kb/corpus/canon
  files parsed:     8
  total wallclock:  1.61ms
  parse μs/file:    mean=189  p50=170  p95=437
------------------------------------------------------------

[3/4] Stage 100-file synthetic blow-up...
  canon sources: 8 files
  staged 104 synthetic files at /tmp/tmp.s0CJP2WsOq-spike-walker

[4/4] Run against synthetic blow-up...
  total wallclock (spawn + IO + parse): 114ms
  CSV rows: 104

  binary's own summary:
    spike-walker summary:
      corpus:           /tmp/tmp.s0CJP2WsOq-spike-walker
      files parsed:     104
      total wallclock:  17.55ms
      parse μs/file:    mean=159  p50=155  p95=294

OPERATOR CHECKLIST:
[x] Run completed with exit code 0 (no panics)
[x] Canon run printed >=5 CSV data rows (8 rows)
[x] 100-file run printed ~104 CSV data rows
[x] All 6 fields populated for every row (spot-check confirmed)
[x] Binary summary 'total wallclock' < 1000ms (17.55ms — 57× headroom)
[x] Parse μs/file p95 < 5000 (294 μs — 17× headroom)
[x] CSV at /tmp/spike-walker.csv; summary at /tmp/spike-walker.summary
```

### What I would NOT carry forward
- The CSV output format is for human spot-checking only — production should emit indexed records directly into lance, not stringify-then-parse.
- The `Selector::parse(...).expect(...)` pattern with re-parsing on every call is wasteful — production should cache selectors in a `static OnceLock` (or build a `LazyLock<ParserSet>`). The spike's ~150 μs/file is already cheap enough not to bother in throwaway code, but at production volume (50k docs × 6 selectors = 300k re-parses on a full reindex) the savings are worth claiming.
- The `corpus_dir` fallback chain (env var → manifest-relative default) is fine for a spike — production resolution should come from `kb.toml` config, not env vars.


---

## spike-fastembed · 2026-05-11

**Status:** partially-worked — model loads, embeds correctly, but **perf does not meet research targets** on this hardware
**Effort:** 1h estimated, ~2h actual (including ~30min runaway batch-128 run before kill)
**Crate hash:** spike branch `spike/fastembed`
**Research source:** `docs/research/02-fastembed.html`
**Host:** Intel i7-7700 (4c/8t @ 3.60GHz, x86_64) · system onnxruntime 1.24.4 (Arch `onnxruntime-cpu`)

### Goal
Measure on this machine the actual latency / throughput / memory / model-size cost of `fastembed` + `bge-small-en-v1.5`. Validate (or refute) topic 02's predicted bounds for x86: 15–40 ms/embed warm, 300–800 emb/s batched.

### Worked as expected
- `fastembed` (5.13.4) constructs `TextEmbedding` against system `onnxruntime` via the `ort-load-dynamic` feature with `ORT_DYLIB_PATH=/usr/lib/libonnxruntime.so`. No bundled binaries, no download of ORT.
- Both model variants download and load cleanly on first construction: `Xenova/bge-small-en-v1.5` (fp32) and `Qdrant/bge-small-en-v1.5-onnx-Q` (quantised).
- Single embeds return 384-dim Vec<f32> as expected.
- Cold start (model already cached): **3.36 s** — well under the 5 s target.

### Surprised
- **Perf is 3-5× slower than research bounds.** Single-embed p50 for ~500-token (2000-char) input is **169.7 ms**, not the 15-40 ms the research predicted for x86. This is a real spike finding that should redirect production planning.
- **Batching does not help.** Per-doc time at batch 8 (189 ms) and batch 32 (177 ms) is *the same as* single-embed time (169 ms). Throughput stays at ~5.6 emb/sec regardless of batch size, vs. the predicted 200+ emb/sec.
- **Quantised is slower than fp32.** Quant p50 = 199 ms vs fp32 p50 = 169 ms. The i7-7700 (Kaby Lake, 2017) lacks AVX-512 / VNNI, which int8 inference typically depends on for speedup. The quantised path's only win on this hardware is disk + memory size (64 MB vs 127 MB), not latency.
- **Batch 128 was killed at 30 min wallclock.** Per-doc throughput collapses at large batch sizes — likely cache-thrashing the L2/L3 working set. Production should cap batch size at 32 on similar hardware.
- **`fastembed` uses its own cache dir.** Models land in `<cwd>/.fastembed_cache/` rather than the standard HuggingFace cache at `~/.cache/huggingface/hub/`. Important for kb's production data-flow planning (XDG cache layout per topic 02 needs explicit `cache_dir` config in `InitOptions`).
- **Default thread config gives no batch parallelism.** Single-thread CPU utilisation during one embed peaked at one full core; multi-doc batches did not light up additional cores. The `ort-load-dynamic` path may need explicit `ExecutionProvider::CPU(...)` configuration with thread hints to get inter-op parallelism.

### Failed / blocked
- Batch 128 run did not complete within reasonable wallclock (>30 min, killed). Not a fastembed bug — a CPU-saturation / memory-bandwidth limit. Production should not attempt very large batches on this class of hardware.

### Measurements
| metric                              | expected (research)   | observed (this machine)              | verdict |
|-------------------------------------|-----------------------|--------------------------------------|---------|
| Cold start (cached)                 | < 5 s                 | **3.36 s**                            | ✅ |
| Cold start (with download)          | < 60 s                | ~25 s (estimated; not measured)       | ✅ likely |
| Warm p50 (~100-tok, 400 char)       | < 50 ms (extrapolate) | 57.7 ms                              | ⚠️ close but over |
| Warm p50 (~500-tok, 2000 char)      | < 50 ms (canonical)   | **169.7 ms** — 3.4× over             | ❌ |
| Warm p50 (~2000-tok, 8000 char)     | < 200 ms (estimate)   | 183.7 ms                              | ✅ surprisingly |
| Batch 8 throughput                  | > 200 emb/sec         | **5.3 emb/sec** — 38× under          | ❌ |
| Batch 32 throughput                 | > 300 emb/sec         | **5.6 emb/sec** — 53× under          | ❌ |
| Batch 128 throughput                | > 500 emb/sec         | killed at 30min                       | ❌ unmeasurable |
| Quantised p50 (~500-tok)            | ≤ fp32 latency        | **199 ms** (vs fp32 169 ms — slower) | ❌ |
| Model file size (fp32)              | ~130 MB               | **127 MB** ✓                          | ✅ |
| Model file size (int8 quantised)    | ~33 MB                | **64 MB** (`*-onnx-Q`)                | ⚠️ closer to fp16/mixed than pure int8 |
| RSS during fp32 inference          | < 500 MB              | 240 MB                                | ✅ |
| RSS during batch 32                 | < 1 GB                | 1.3 GB                                | ⚠️ |

### Architecture decision updates

1. **`02-fastembed.html` §Decisions — REVISED.** The research's perf bounds (15–40 ms/embed x86, 300–800 emb/sec batched) are aspirational for a Kaby Lake-class CPU. On i7-7700 we get ~170 ms/embed and ~6 emb/sec batched. **Action:** kb-core's indexing pipeline must plan for **slow embedding on personal-collection hardware**. For 50k artifacts, full reindex is ~140 min, not 1–3 min. Embedding parallelism (one daemon, one model instance, sequential embeds) is the bottleneck — not lance, not the parser. Production should:
   - Persist embeddings across runs (no full reindex on startup).
   - Embed incrementally as new artifacts appear (via the file watcher); avoid bulk reembed except when model changes.
   - Surface progress via SSE so the TUI shows the slow indexing transparently — the existing `index.file` event is sufficient.

2. **`02-fastembed.html` §"Pin fastembed = 5.12" — REVISED to `version = "5"`.** Spike used fastembed 5.13.4 (latest as of 2026-05-11) without issue. Pinning to exactly `5.12` is unnecessarily strict; minor-flex within `5.x` is fine.

3. **`02-fastembed.html` Decision: "default to fp32, quantised on request" — REVISED.** On hardware without AVX-512/VNNI (anything pre-Ice Lake / pre-Rocket Lake, plus all current AMD without VNNI), quantisation **does not improve latency** and may degrade quality. **Action:** kb-core's `kb model {download,set,list}` CLI should NOT default to int8 quantised. Default to fp32. Document the quant variant as a memory-savings option for memory-constrained hosts only.

4. **`02-fastembed.html` §"ort with load-dynamic" — CONFIRMED.** The `ort-load-dynamic` feature works against system onnxruntime 1.24.4 (the version `ort 2.0-rc.12` officially supports). System install via `pacman -S onnxruntime-cpu` is the simplest path on Arch; CI uses the upstream 1.20.1 tarball. **NEW:** ORT 1.24 is compatible despite the research mentioning 1.16-1.20; this is good for kb's production deps.

5. **NEW — cache directory needs explicit configuration.** fastembed creates `./<cwd>/.fastembed_cache/` by default, NOT `~/.cache/huggingface/hub/`. Topic 02 specifies `~/.cache/kb/models/<name>/<hf-revision>/` for the production layout. **Action:** kb-core must construct `InitOptions::new(...).with_cache_dir(xdg::cache_home("kb/models"))` explicitly. Without this, kb will create cache directories wherever the daemon happens to be invoked.

6. **NEW — batch sizes should be capped.** On this hardware, batches >32 thrash. Production embedding pipeline should batch at 16 or 32 max, never 128+. **Action:** `kb-core::indexer` ingests artifacts in batches of 32 max regardless of pending-queue size.

### Manual procedure log

```
$ ORT_DYLIB_PATH=/usr/lib/libonnxruntime.so bash scripts/manual-run.sh
[1/4] Pre-flight...
  ORT_DYLIB_PATH=/usr/lib/libonnxruntime.so
  HF model cache: ~/.cache/huggingface/hub
  (cache empty — first run includes model download ~33MB int8)
[2/4] Build release binary...
[3/4] Run full bench suite...
  (interrupted by operator at batch_size=128 step after 30+ min wallclock)
[4/4] Partial results:

{"kind":"cold","model":"bge-small-en-v1.5","input_chars":null,"batch_size":null,"runs":1,"p50_ms":3361.5,"mean_ms":3361.5,"rss_kb_before":6004,"rss_kb_after":212252,"notes":"construction time (model was cached at try_new)"}
{"kind":"warm","model":"bge-small-en-v1.5","input_chars":400,"runs":20,"p50_ms":57.7,"p95_ms":136.2,"throughput_embeds_per_sec":17.3,"rss_kb_after":224728}
{"kind":"warm","model":"bge-small-en-v1.5","input_chars":2000,"runs":20,"p50_ms":169.7,"p95_ms":355.9,"throughput_embeds_per_sec":5.9,"rss_kb_after":240740}
{"kind":"warm","model":"bge-small-en-v1.5","input_chars":8000,"runs":20,"p50_ms":183.7,"p95_ms":382.2,"throughput_embeds_per_sec":5.4,"rss_kb_after":238124}
{"kind":"batch","model":"bge-small-en-v1.5","input_chars":2000,"batch_size":8,"runs":10,"p50_ms":1519.1,"throughput_embeds_per_sec":5.3,"notes":"per-doc 189.88ms","rss_kb_after":479940}
{"kind":"batch","model":"bge-small-en-v1.5","input_chars":2000,"batch_size":32,"runs":10,"p50_ms":5677.97,"throughput_embeds_per_sec":5.64,"notes":"per-doc 177.44ms","rss_kb_after":1295692}
{"kind":"quant","model":"bge-small-en-v1.5-q (int8)","input_chars":2000,"runs":20,"p50_ms":199.4,"throughput_embeds_per_sec":5.0,"rss_kb_after":326520}

Model file sizes (post-download):
  fp32:        127 MB  (`Xenova/bge-small-en-v1.5/onnx/model.onnx`)
  quantised:    64 MB  (`Qdrant/bge-small-en-v1.5-onnx-Q/model_optimized.onnx`)
  total cache: 192 MB  (`spikes/fastembed/.fastembed_cache/`)

OPERATOR CHECKLIST:
[x] No process crash or onnxruntime dlopen failure
[x] Cold start: < 5s if cache populated — 3.36s
[ ] warm 2000ch p50 < 50ms — 169.7ms (FAIL, 3.4× over)
[ ] batch 32 throughput > 200 embeds/sec — 5.64 emb/s (FAIL, 35× under)
[ ] Quant variant p50 < fp32 p50 — 199 vs 169 (FAIL on Kaby Lake)
[x] RSS during inference < 500MB — peaked at 1.3GB at batch 32 (over for big batches)
```

### What I would NOT carry forward

- The `cargo run` per-subcommand pattern for the bench. Production benchmarks should be Criterion-based (with proper warmup/sampling).
- `fastembed = "5"` with `default-features = false` and `["ort-load-dynamic", "hf-hub-native-tls"]` is the right feature set for the daemon — but the spike's main.rs's pattern of `TextEmbedding::try_new(...)` per benchmark step is wasteful (model loads 5+ times). Production loads once and reuses.
- `text_of_length` synthetic input is fine for spike measurement; production benchmarks should use real artifact text (sliced from corpus/canon) so token-distribution effects show up.
- The implicit batch-size scaling assumption (8 → 32 → 128 → 512) — verified bad on this hardware. Production indexer should cap at 32.


---

## spike-sse · 2026-05-11

**Status:** worked
**Effort:** 2h estimated, ~1.5h actual
**Crate hash:** spike branch `spike/sse`
**Research source:** `docs/research/04-sse-axum.html`

### Goal
Confirm `broadcast::channel` + `VecDeque` ring + `Last-Event-ID` replay produces no duplicates and no gaps on reconnect. Verify subscribe-first-then-snapshot-with-id-filter is the right ordering.

### Worked as expected
- The **subscribe-first then snapshot then dedupe-by-id** pattern eliminates both the gap and duplicate cases:
  1. Subscribe a live receiver before reading the ring.
  2. Snapshot all events with `id > last_event_id`.
  3. In the live stream, drop any event where `id <= max(snapshot.id)` (it was already replayed).
- Curl reconnect with `Last-Event-ID: 3` cleanly resumes at id 4 with no skips and no dups, exactly as the research's recipe predicted.
- `BroadcastStreamRecvError::Lagged` is surfaced as a synthetic `event: lag` to clients — the pattern is the same as topic 04's snippet.
- `KeepAlive::new().interval(Duration::from_secs(15)).text(":keep-alive")` fires the SSE `:`-prefixed comment line every 15 s on idle connections. Verified at unit level; manual operator confirmed by holding `curl -N` for 20+ seconds.
- The `EventBus` is unit-testable in pure code: 5 unit tests cover monotonic ids, ring eviction at capacity, snapshot filtering, live subscription, and the subscribe-then-snapshot dedupe invariant.
- The full axum app boots and serves on `127.0.0.1:0` (kernel-assigned port) in the integration test, with `reqwest-eventsource` confirming reconnect-with-replay end-to-end on real network sockets.

### Surprised
- `axum 0.8`'s `Sse::keep_alive(...)` accepts a `.text(":keep-alive")` to customise the comment payload — undocumented in the research but useful for grepping ops logs.
- The integration test pattern `let (listener, addr) = TcpListener::bind("127.0.0.1:0").await; let handle = tokio::spawn(axum::serve(...))` makes it trivial to write end-to-end SSE tests without port collisions. This is the right pattern for production `kb-server` tests too.
- `reqwest-eventsource` does NOT automatically send `Last-Event-ID` on reconnect when used via `EventSource::new(reqwest::RequestBuilder)`. The header must be set explicitly. Production clients must implement Last-Event-ID tracking themselves (or use a higher-level wrapper).
- Sharing the SSE handler between `main.rs` and tests required duplicating the handler in `lib::test_support` because `bin` modules can't be imported by `tests/*`. This is a small papercut — production should put the handler in `kb-core` and import from both `kb-server` and tests.

### Failed / blocked
- N/A. All success criteria from `spikes.html` met.

### Measurements
| metric                            | expected (research)        | observed                                   | verdict |
|-----------------------------------|----------------------------|--------------------------------------------|---------|
| Reconnect-with-replay no dups     | none                       | 0 across 3 integration tests + manual run  | ✅ |
| Reconnect-with-replay no skips    | none                       | 0 — exact event ids `[8,9,10,11,12]` after `Last-Event-ID: 7` | ✅ |
| Heartbeat interval                | 15s ± jitter               | 15s comment lines confirmed                 | ✅ |
| Lag event on full broadcast       | `event: lag` w/ miss count | unit-level: `BroadcastStreamRecvError::Lagged(n)` → `event:lag` with `{"missed":n}` payload | ✅ |
| Subscribe-first ordering          | dedupes vs snapshot-first  | unit test `subscribe_then_snapshot_avoids_dups_via_id_filter` passes | ✅ |

### Architecture decision updates

1. **`04-sse-axum.html` recipe — CONFIRMED.** The broadcast + VecDeque + Last-Event-ID pattern is copy-pasteable into `kb-server`. **Action:** carry forward `EventBus` (rename to `kb_core::events::EventBus`) as the canonical event firehose. Ring capacity 1024 and broadcast channel capacity 256 are good defaults for production.

2. **`04-sse-axum.html` ordering question — RESOLVED.** Subscribe-first, then snapshot, then `id`-filter is correct. **Action:** the production handler in `kb-server` follows this ordering verbatim. Documented as an invariant in the handler comment.

3. **`04-sse-axum.html` lag-handling — CONFIRMED.** Synthesising an `event: lag` SSE frame on `BroadcastStreamRecvError::Lagged(n)` lets clients detect "you missed N events" and recover (typically: reload state from a snapshot endpoint). **Action:** carry forward.

4. **NEW — handler should live in `kb-core`, not `kb-server`.** Tests need to import it, and `bin` modules can't be imported. Production layout: `kb_core::sse::events_handler(...)` exposed as a public function that takes `State<Arc<EventBus>>` and is mounted by `kb-server`'s router. This avoids the test-support duplication seen in this spike.

5. **NEW — `Last-Event-ID` is a client responsibility.** `reqwest-eventsource` (and browser `EventSource`) only automatically retry the connection; tracking the last received id and re-sending the header is on the client. **Action:** the TUI and SPA both need to maintain a `last_event_id` cell that's updated as events stream in, and re-injected on reconnect.

### Manual procedure log
```
[1/4] Build release binary... (cargo build --release succeeded)
[2/4] Launch server in background on 127.0.0.1:7001...
  server PID=1200394, logs at /tmp/spike-sse.log

[3/4] Collect first 5 events from a fresh connection:
    id: 1   event: tick   data: {"counter":1}
    id: 2   event: tick   data: {"counter":2}
    id: 3   event: tick   data: {"counter":3}
    id: 4   event: tick   data: {"counter":4}
    id: 5   event: tick   data: {"counter":5}
    id: 6   event: tick   data: {"counter":6}
    id: 7   ...

[4/4] Reconnect with Last-Event-ID: 3 — events 4 onward only:
    id: 4   event: tick   data: {"counter":4}     ← first id > 3
    id: 5   event: tick   data: {"counter":5}
    id: 6   event: tick   data: {"counter":6}
    id: 7   event: tick   data: {"counter":7}
    id: 8   event: tick   data: {"counter":8}
    id: 9   event: tick   data: {"counter":9}
    id: 10  ...

OPERATOR CHECKLIST:
[x] Server started cleanly
[x] Step 3 events have ids 1..N (fresh client gets full ring snapshot)
[x] Step 4 events have ids strictly > 3 (no replay of 1,2,3)
[x] No duplicate id across step 3 or step 4
[x] Heartbeat ': keep-alive' fires every 15s on idle (confirmed by code + 20s idle hold)

Optional (verified via integration tests instead of manually):
[x] reqwest-eventsource reconnect with Last-Event-ID produces exact id sequence
[x] BroadcastStreamRecvError::Lagged → synthetic event:lag (verified in lib unit test)
```

### What I would NOT carry forward
- The `lib::test_support` duplicate of the SSE handler — papercut from `bin` modules not being importable. Production puts the handler in `kb-core` and both `kb-server` and tests import from there.
- The `axum::extract::State<Arc<EventBus>>` pattern is fine for a single-tenant daemon but production must support multi-kb federation (one bus per kb). The `State` type will be wider (e.g., `State<KbHandles>` carrying a `HashMap<KbName, Arc<EventBus>>`).
- The `KB_SSE_ADDR` env-var binding is a spike convenience; production uses `kb.toml` and a CLI flag.
- The `tick_task` is purely for spike behaviour — production indexer drives the bus from real file-watch events.


---

## spike-iframe · 2026-05-11

**Status:** worked (Chromium fully validated via Playwright; Firefox manual operator test deferred)
**Effort:** 3h estimated, ~2h actual
**Crate hash:** spike branch `spike/iframe`
**Research source:** `docs/research/06-iframe.html` (cluster-1 resolution)
**Host:** Arch Linux, Chromium via Playwright MCP, system glibc resolver

### Goal
Validate the cluster-1 resolution from topic 06: serve each artifact from a distinct subdomain `<id>.artifacts.localhost:7000` (so each iframe gets a real, isolated `Origin`), with sandbox attribute `allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox allow-forms allow-modals allow-downloads`. The crucial cluster-1 correction is including `allow-same-origin` — origin-level isolation comes from the **subdomain**, not from removing `allow-same-origin` (which would break `localStorage`).

### Worked as expected

- **`*.localhost` wildcard DNS** resolves out of the box on Arch Linux with system glibc/systemd-resolved (RFC 6761). `curl -v http://s01.artifacts.localhost:7777` resolves to `::1` and `127.0.0.1` without any `/etc/hosts` or dnsmasq configuration. No fallback needed.
- **Host-header dispatch** works exactly as designed: `Host: <id>.artifacts.localhost:7000` → serves `corpus/canon/<id>.html` (with probe injected). Other hosts (`localhost:7000`, `127.0.0.1:7000`) → parent test page with one iframe per canon artifact.
- **Probe-script injection** at serve time succeeds for all 4 canon artifacts. Injection point is `</body>` (or end of doc if no body close). Production should use `lol-html` for streaming rewrites; for the spike, simple text replacement is sufficient.
- **All 4 browser-level probes PASS on all 4 artifacts in Chromium** (verified via Playwright):

  | artifact | localStorage | parent.document access | sandbox-removal | direct API fetch |
  |---|---|---|---|---|
  | multi-page | ✅ wrote `hello-1778510366258` | ✅ SecurityError | ✅ denied | ✅ CORS-blocked |
  | cost-of-abstraction | ✅ wrote `hello-1778510366342` | ✅ SecurityError | ✅ denied | ✅ CORS-blocked |
  | fullscreen-viz | ✅ wrote `hello-1778510366345` | ✅ SecurityError | ✅ denied | ✅ CORS-blocked |
  | kitchen-sink | ✅ wrote `hello-1778510366373` | ✅ SecurityError | ✅ denied | ✅ CORS-blocked |

- **Per-origin storage isolation** is confirmed indirectly: each artifact wrote a distinct `hello-<timestamp>` to its own `localStorage`, the parent origin (`http://localhost:7000`) has `null` for that key. Different subdomains → different Origins → fully isolated `localStorage`.
- **7/7 programmatic dispatch checks pass** via curl with explicit `Host:` headers (parent vs subdomain, 404 for unknown ids, headers correct, probe.js served at `/_kb/probe.js`, `api/health` reachable from parent origin).

### Surprised

- The Playwright-driven probe approach turned out to be a strong validation tool for spikes that need browser-level behaviour. **Action:** for production, build a Playwright-based smoke test that boots `kb-server` and verifies the same 4 probes across N artifacts on every CI run.
- Topic 06 mentioned `dnsmasq fallback` (`address=/.localhost/127.0.0.1`) as a contingency. On this Arch box (glibc 2.41 + systemd-resolved 257) the fallback was unnecessary — the wildcard worked out of the box.
- Chromium fires the iframes' probe scripts in **arbitrary order** (multi-page first, then cost-of-abstraction, then fullscreen-viz, then kitchen-sink). Production code should not assume iframe load order matters for postMessage handlers.

### Failed / blocked

- **Firefox manual operator test deferred** — Playwright defaults to Chromium. The script's operator checklist still calls for Firefox verification. Risk is low (RFC 6761 + same sandbox flags should yield equivalent behaviour) but should be ticked off before kb-server ships. **Action:** add to roll-up follow-ups.
- **Safari ESR not in scope on Linux.** Topic 06 already flagged Safari as a separate spike. Defer until the v0.0.1 → v0.1 transition.

### Measurements

| metric                              | expected (research)                | observed in Chromium                | verdict |
|-------------------------------------|------------------------------------|-------------------------------------|---------|
| `*.localhost` resolution            | works on modern glibc              | works (no /etc/hosts, no dnsmasq)   | ✅ |
| Host-header dispatch                | parent vs subdomain                | 7/7 programmatic checks pass         | ✅ |
| Probe injection                     | before `</body>`                   | confirmed in 4/4 artifacts          | ✅ |
| `localStorage` per-subdomain         | isolated across artifacts          | 4 distinct values, parent has none  | ✅ |
| Cross-origin parent.document access | SecurityError                       | 4/4 SecurityError as expected       | ✅ |
| Sandbox-removal attack              | fails (parent doc unreachable)     | 4/4 SecurityError                   | ✅ |
| Direct fetch to /api from iframe    | CORS-blocked                       | 4/4 blocked (no `Access-Control-Allow-Origin`) | ✅ |
| Chromium                            | all 4 probes PASS                  | **16/16 PASS** (4 artifacts × 4)    | ✅ |
| Firefox                             | all 4 probes PASS                  | deferred — Playwright was Chromium  | ⏳ |

### Architecture decision updates

1. **`06-iframe.html` cluster-1 resolution — FULLY CONFIRMED.** The subdomain-per-artifact pattern with `allow-same-origin` in the sandbox attribute is **correct, working, and copy-pasteable** into `kb-server`. **Action:** carry forward `parse_artifact_id`, `inject_probe_script`, and `SANDBOX_FLAGS` into `kb-core::iframe` as production primitives. Use `lol-html` (not text replacement) for production injection.

2. **NEW — `*.localhost` works without fallback on Arch.** The dnsmasq contingency from topic 06 is unnecessary in the user's environment. **Action:** document in production install guide that the wildcard works out of the box on modern glibc; only recommend dnsmasq if `getent hosts s01.artifacts.localhost` fails.

3. **NEW — host parser must reject leading/trailing dots.** Initial implementation accepted `..artifacts.localhost` because the suffix-strip yielded `.` and the chars-allowed test passed. Production parser explicitly rejects `.`-starts/`.`-ends/`..`-contains. **Action:** carry forward the strict parser (and its unit tests) into `kb-core`.

4. **NEW — postMessage probe pattern is the right tool for browser spikes.** Inject probe → postMessage results to parent → parent displays in `<pre>`. **Action:** keep this pattern for kb-server's CI smoke test against real browsers via Playwright.

5. **NEW — Firefox parity needs verification before shipping.** No reason to expect divergence (Firefox 84+ supports `*.localhost` per RFC 6761), but should be operator-confirmed once. **Action:** add to v0.0.1 ship checklist.

### Manual procedure log

```
$ getent hosts s01.artifacts.localhost
::1             localhost
(resolves OK — RFC 6761)

$ bash scripts/manual-run.sh
[PASS] parent page served at /
[PASS] subdomain serves canon artifact (kitchen-sink)
[PASS] subdomain injects probe script
[PASS] unknown subdomain returns 404
[PASS] probe.js is served at /_kb/probe.js
[PASS] X-Kb-Artifact-Id header set on artifact responses
[PASS] api/health reachable directly (parent page would use this)

Playwright (Chromium) probe results from parent log pane:
[PASS] origin=http://multi-page.artifacts.localhost:7000          localStorage: hello-1778510366258
[PASS] origin=http://multi-page.artifacts.localhost:7000          cross_origin_parent_doc_access: SecurityError as expected
[PASS] origin=http://multi-page.artifacts.localhost:7000          sandbox_removal_attack: SecurityError (correctly denied)
[PASS] origin=http://multi-page.artifacts.localhost:7000          direct_api_fetch: blocked: Failed to fetch
[PASS] origin=http://cost-of-abstraction.artifacts.localhost:7000  localStorage: hello-1778510366342
[PASS] origin=http://cost-of-abstraction.artifacts.localhost:7000  cross_origin_parent_doc_access: SecurityError
[PASS] origin=http://cost-of-abstraction.artifacts.localhost:7000  sandbox_removal_attack: denied
[PASS] origin=http://cost-of-abstraction.artifacts.localhost:7000  direct_api_fetch: blocked
[PASS] origin=http://fullscreen-viz.artifacts.localhost:7000      localStorage: hello-1778510366345
[PASS] origin=http://fullscreen-viz.artifacts.localhost:7000      cross_origin_parent_doc_access: SecurityError
[PASS] origin=http://fullscreen-viz.artifacts.localhost:7000      sandbox_removal_attack: denied
[PASS] origin=http://fullscreen-viz.artifacts.localhost:7000      direct_api_fetch: blocked
[PASS] origin=http://kitchen-sink.artifacts.localhost:7000        localStorage: hello-1778510366373
[PASS] origin=http://kitchen-sink.artifacts.localhost:7000        cross_origin_parent_doc_access: SecurityError
[PASS] origin=http://kitchen-sink.artifacts.localhost:7000        sandbox_removal_attack: denied
[PASS] origin=http://kitchen-sink.artifacts.localhost:7000        direct_api_fetch: blocked

Parent origin localStorage probe key value: null  → confirms per-origin isolation.

CHECKLIST:
[x] All 4 probes PASS in Chromium for all 4 artifacts (16/16)
[ ] All 4 probes PASS in Firefox  ← OPERATOR TODO before v0.0.1 ship
[x] Per-artifact localStorage values are distinct (4 unique timestamps)
[x] Parent origin has no probe value (storage isolation confirmed)
```

### What I would NOT carry forward

- Text-replacement HTML injection (`html.rfind("</body>")`) is fine for a 50-LOC spike but production needs `lol-html` for streaming, large-file-safe rewriting that doesn't allocate the whole body.
- Generating the parent test page in a Rust string literal — production parent is a real React SPA, not server-rendered HTML.
- The route `/api/health` returning no `Access-Control-Allow-Origin` is correct (we want CORS to block iframe → parent fetches), but production kb-server needs a clearer policy: parent origin (`http://localhost:7000`) MUST be allowed for the SPA's own API calls; subdomain origins (`http://*.artifacts.localhost:7000`) MUST NOT. Action: spec this explicitly in `kb-core::cors`.
- The `route_placeholder` dead code at the bottom of `main.rs` — remnant of an earlier sketch.


---

## spike-ratatui · 2026-05-11

**Status:** worked (rendering primitives + state machine fully tested; full-UX operator review deferred)
**Effort:** 4h estimated, ~1.5h actual
**Crate hash:** spike branch `spike/ratatui`
**Research source:** `docs/research/05-ratatui.html`

### Goal
Validate the 3-task pattern (input + SSE + tick → mpsc) + 30 Hz dirty-gated render + Component trait against spike-sse's counter feed.

### Worked as expected
- **Component trait** (`fn render(&self, area, buf, state)`) is a clean, scalable shape. Three components implemented (Header / Body / Footer); pattern would scale to 6 (kb-tui's planned HOME / SOURCES / DETAIL / STATS / ERRORS / SEARCH tabs) without surgery.
- **3-task pattern** with `tokio::spawn` feeding a single `mpsc::UnboundedSender<AppEvent>` channel works as topic 05's recipe predicted. Each task is independent and crash-isolated.
- **Dirty-gate at 30 Hz** is the right primitive for keeping idle CPU low. The state-machine excludes `anim_step` from the dirty-snapshot, so the throbber-style tick doesn't trigger repaint. **Verified in unit test** `state_apply_anim_tick_does_not_force_repaint` — 100 AnimTick events with state otherwise stable → 0 dirty marks.
- **Pseudo-TTY rendering works**: launched via `script` (which allocates a pty), the binary emits the expected alt-screen + cursor-hide ANSI sequences and renders to the pty buffer. The full UX is operator-verifiable but the wiring is correct.
- **ratatui's `TestBackend`** is the right tool for headless integration testing. Five tests render the full UI (header + body + footer) into an in-memory `Buffer` and assert text content, validating that:
  - Initial state renders "connecting" + counter=0.
  - SseTick transitions to "live" with the right counter + last_event_id.
  - SseStateChange(Disconnected) renders "DISCONNECTED".
  - Dirty-gate excludes AnimTick.
  - Same counter with new event_id IS dirty (reconnect-replay correctness).

### Surprised
- crossterm's `enable_raw_mode` + `EnterAlternateScreen` fails with `os error 6` (`No such device or address`) when stdout is NOT a terminal. The error is reasonable but the binary needs `script -q -c '…'` or a real terminal to run. Production `kb-tui` should detect this and print a friendly "must be run in a terminal" error instead of the raw OS error.
- `ratatui::Buffer::cell((x, y))` returns `Option<&Cell>` (not `&Cell` directly) — the unit-test buffer-dump helper had to `.map(|c| c.symbol()).unwrap_or("")`. Minor API ergonomics paper-cut.

### Failed / blocked
- **Live `KB_SSE_URL` validation deferred to operator.** The crossterm-requires-TTY constraint means I can't easily run the full app + verify counter increments + verify reconnect UX from this environment. The unit + integration tests prove the rendering & state-machine logic; the operator should run `just spike ratatui` in a real terminal alongside `just spike sse` and tick the manual checklist.
- **<1% idle CPU verification deferred.** The dirty-gate at 30 Hz select-loop will not render at idle, but actual `top`/`htop` verification requires the operator. The state-machine guarantee (anim_tick excluded from dirty-snapshot) is the architectural one; CPU measurement is the empirical confirmation.

### Measurements

| metric                                    | expected (research)         | observed                                  | verdict |
|-------------------------------------------|------------------------------|-------------------------------------------|---------|
| Component trait scales to 6               | yes                          | yes (pattern unchanged at 3, extensible)  | ✅ |
| Dirty-gate excludes anim_tick             | yes                          | yes (verified via state machine test)     | ✅ |
| Render via TestBackend produces correct text | matches expected line     | all 5 integration tests pass              | ✅ |
| Counter visible <33ms after SSE event     | <33ms                        | architecturally yes (mpsc → render loop)  | ⏳ operator-confirm |
| Idle CPU <1%                              | <1%                          | architecturally yes (dirty-gate logic)    | ⏳ operator-confirm |
| Reconnect-with-replay no missed counters  | none                         | architecturally yes (Last-Event-ID in EventSource) | ⏳ operator-confirm |
| Binary actually renders in a pty          | yes                          | alt-screen + cursor-hide ANSI emitted     | ✅ |

### Architecture decision updates

1. **`05-ratatui.html` "Concrete recommendation" — CONFIRMED.** The 3-task / mpsc / dirty-gate / Component pattern is copy-pasteable into `kb-tui`. **Action:** carry forward `AppEvent`, `AppState`, `Component`, `Header`, `Body`, `Footer` into `kb-tui` (renamed `kb_tui::ui::{…}`).

2. **NEW — dirty-snapshot tuple must exclude anim_step.** The dirty-gate is only effective when the state-shape used to detect change excludes purely cosmetic counters (throbber spin, blinking cursor, etc.). **Action:** document this invariant in the production AppState comment.

3. **NEW — production must detect non-TTY and error early.** Bare `os error 6` is unfriendly. **Action:** kb-tui's `setup_terminal` should test `std::io::stdout().is_terminal()` first and print "must be run in a terminal (try `kb tui` from inside an interactive shell)".

4. **NEW — TestBackend is the right tool for kb-tui's CI.** Render every tab into a TestBackend buffer + insta snapshot the text. CI catches regressions in the ratatui markup without needing a terminal. **Action:** carry over into kb-tui's tests.

5. **NEW — pseudo-tty smoke test for the binary.** A shell test that does `script -q -c './target/release/kb-tui' /tmp/ratatui.log` for 3 seconds, then asserts the log contains the alt-screen escape sequence, gives us "binary launches in a real-tty environment" coverage without needing CI to allocate a terminal. **Action:** add to kb-tui's CI.

### Manual procedure log

```
# Unit tests (lib + integration)
$ cargo test
running 4 tests in lib + 5 tests in integration → ALL PASS (9/9)

# Compile + headless smoke launch (pseudo-tty via `script`)
$ KB_SSE_ADDR=127.0.0.1:7002 ./../sse/target/release/spike-sse &
$ script -q -c "env KB_SSE_URL=http://127.0.0.1:7002/events ./target/release/spike-ratatui" /tmp/ratatui-script.log
$ cat /tmp/ratatui-script.log
[?1049h  (alt-screen enter)
[?25l   (cursor hide)
[39m[49m  (default fg/bg) — repeated render frames
…
(binary renders alt-screen + cursor-hide as expected; CONFIRMS the terminal-setup chain works)

# Without a pty:
$ ./target/release/spike-ratatui
Error: No such device or address (os error 6)
(expected — crossterm needs a real TTY)

OPERATOR CHECKLIST (perform in a real terminal):
[ ] just spike sse  in pane 1
[ ] just spike ratatui  in pane 2
[ ] header shows 'counter=N' incrementing every 1s; last_event_id increments too
[ ] footer dot is GREEN '●' while sse is alive
[ ] htop -p $(pgrep spike-ratatui)  shows <1% idle CPU
[ ] kill sse → footer turns RED '✕', body shows 'DISCONNECTED — will retry'
[ ] restart sse → reconnects within ~2s, counter resumes (starts over from 1
    because spike-sse doesn't persist state — but the Last-Event-ID header IS sent)
[ ] q quits cleanly; terminal restored (alt-screen left, cursor visible, no garbage)
```

### What I would NOT carry forward
- The `format!(" kb · spike-ratatui ")` no-arg format string (clippy caught this — production code should just use `&str` literals).
- The `Body` component rendering only one status line — production `kb-tui` Body needs to be the actual content area (artifact list, search results, atlas) for each of the 6 tabs.
- spike-sse's counter as a state shape — production `kb-server` emits 17 canonical event types per `04-sse-axum.html`. The `AppEvent` enum will be much larger; carry the dirty-gate pattern but expect the snapshot tuple to be wider.
- `Header`/`Body`/`Footer` as unit structs — production should use real state-bearing structs (e.g., `BodyTabState` with which-tab-is-active, scroll position, etc.).


---

## spike-lance · 2026-05-11

**Status:** **worked** — full hybrid-search pipeline + schema evolution validated
**Effort:** 4h estimated, ~3h actual (substantial time on arrow version drift before unblocking)
**Crate hash:** spike branch `spike/lance`
**Research source:** `docs/research/01-lancedb.html`

### Goal
Validate lancedb 0.27.2's Rust crate end-to-end: schema with `FixedSizeList<Float32, 384>`, FTS index for BM25, vector queries, RRF hybrid query, cold-open latency, and the schema-evolution path (bug #3136).

### Worked as expected
- **Full CRUD-and-query pipeline runs cleanly**: schema → `create_table` from `Vec<RecordBatch>` → FTS index creation → BM25 query → vector query (with `nearest_to` + IVF-PQ implicit) → hybrid query (BM25 + vector concurrently). 10 integration tests pass.
- **Schema construction** with `arrow::datatypes::Schema` and `FixedSizeList<Float32, 384>` matches topic 01's spec exactly. The four-field shape (`id`, `title`, `body`, `embedding`) works without surgery.
- **BM25 query** finds the expected match: `body LIKE '%borrow%'` → "Visualizing the Borrow Checker" (1 row, 10.4 ms).
- **Vector query** with toy deterministic embedding ranks the exact-match document first in the integration test (`vector_query_ranks_exact_match_first` is green).
- **Hybrid query** (FTS + vector in a single `.query()` chain) returns 5 results combining both signals — research's "RRF reranker is built-in" claim is confirmed via the lancedb 0.27 API.
- **Cold-open <1 ms** for a 6-row dataset with FTS+IVF-PQ indices on this hardware (i7-7700, NVMe SSD). Vastly under research's "<100ms" target.
- **Schema evolution via `add_columns`** with `NewColumnTransform::SqlExpressions` SUCCEEDS in lance 4.0.0 — bug #3136 **NOT observed** in this version. Adding `kb_category` column post-hoc as a nullable string works.
- **On-disk layout** matches research's description: `_versions/`, `_indices/`, `_transactions/`, `data/` subdirectories all created. 6-row dataset with 384-dim embeddings + FTS index takes 20 KB total.

### Surprised
- **Arrow version drift is the real gotcha.** lance 4.0.0 transitively pulls in arrow 57.3.1. If the spike's direct deps use `arrow = "55"` or `arrow = "56"`, two distinct crate versions coexist in the dep graph and `RecordBatch` types don't unify — `create_table` fails to type-check. **Fix:** pin to *exactly* the version lance uses. `cargo metadata` confirms only one arrow version present in the graph after pinning.
- **`Scannable` accepts `Vec<RecordBatch>` directly** — the simplest possible input type. Don't bother with `RecordBatchIterator` if you can buffer all batches in memory.
- **`FullTextSearchQuery` lives in `lance_index::scalar`**, not `lancedb::query` (the latter privately imports it). `lance-index` must be a direct dependency, pinned to lance's version.
- **`AddColumnsParams` does not exist** — the API is `NewColumnTransform::SqlExpressions(Vec<(String, String)>)` (plural). Earlier attempts using `SqlExpression` (singular) silently fail to compile.
- **`SendableRecordBatchStream` lives at `lancedb::arrow::`**, not `lancedb::query::` (private re-export). The compiler error helpfully suggests the move.
- **lance's compile graph is heavy.** Cold debug build ~3 min, cold release build ~5+ min, fully cached <10s. Production CI must cache `target/` aggressively (Swatinem/rust-cache per spike, already configured).

### Failed / blocked
- N/A. All seven success criteria from `spikes.html` met.

### Measurements

| metric                              | expected (research)                | observed                                  | verdict |
|-------------------------------------|------------------------------------|-------------------------------------------|---------|
| `connect()` on fresh dir            | works                              | ✅                                         | ✅ |
| `connect()` idempotent              | works                              | ✅                                         | ✅ |
| Schema construction (4 fields)      | FixedSizeList<Float32,384> + scalars | ✅                                       | ✅ |
| `create_table` from `Vec<RecordBatch>` | works                          | ✅                                         | ✅ |
| FTS index creation                  | works                              | ✅                                         | ✅ |
| BM25 query                          | returns matches, indices used      | ✅ 1 row for 'borrow' query, 10.4ms      | ✅ |
| Vector query                        | returns ranked nearest             | ✅ exact-match ranked top-1               | ✅ |
| Hybrid (BM25 + vector + RRF)        | both signals contribute            | ✅ 5 mixed-source results, 13.8ms        | ✅ |
| Cold-open latency                   | <100 ms                            | **0.68 ms** (~150× headroom)             | ✅ |
| Schema evolution (`add_columns`)    | yes/no on bug #3136                | **OK** — #3136 **NOT observed** in lance 4.0.0 | ✅ |
| On-disk layout structure            | `_versions/`, `_indices/`, etc.   | ✅ all four expected subdirs              | ✅ |
| 6-row dataset on-disk size          | "few hundred KB" (rough)            | 20 KB total                               | ✅ |
| Cold debug build time               | unspecified                        | ~3 min (deps), <10s warm                  | ⚠️ heavy |

### Architecture decision updates

1. **`01-lancedb.html` — CONFIRMED in full.** lancedb 0.27.2's Rust API works for schema + FTS + vector + hybrid + cold-open + schema evolution. **Action:** carry forward `schema()`, `Doc`, `docs_to_batches()` shape, and the query builder patterns into `kb-core::storage`. Use `lancedb` (not lance directly) for the indexer's primary interface — the high-level API is sufficient.

2. **`01-lancedb.html` bug #3136 — NOT OBSERVED in lance 4.0.0.** The research's caveat about schema evolution past embedding columns no longer applies (or doesn't apply at this version). **Action:** drop the "schema evolution will hit #3136" assumption from production planning. `add_columns(NewColumnTransform::SqlExpressions(...))` is safe.

3. **NEW — arrow version pinning is mandatory.** Production `kb-core` MUST pin `arrow = "=57.3.1"`, `arrow-array = "=57.3.1"`, `arrow-schema = "=57.3.1"` (exact-match) to lance's transitive version. Add a CI check: `cargo metadata --format-version 1 | jq '[.packages[] | select(.name | startswith("arrow")) | .version] | unique | length == 1'`.

4. **NEW — `lance-index` must be a direct dependency.** `FullTextSearchQuery` lives there, not in lancedb's public exports. **Action:** `kb-core::Cargo.toml` includes `lance-index = "=4.0.0"` as a direct dep, version-matched to lance.

5. **NEW — `Vec<RecordBatch>` is the simplest `Scannable` input.** For kb's indexer where artifacts are batched per-watch-event (typically <10 rows at a time), buffering in memory is fine. Don't reach for `RecordBatchIterator` or `Stream`-based variants unless batching truly large datasets.

6. **NEW — disable lancedb's cloud features.** `default-features = false` shaves substantial compile time + binary size by skipping aws/gcs/azure/dynamodb/oss feature gates. kb is local-only; production `kb-core` should opt out of all five.

7. **CONFIRMED — cold-open is essentially free.** Topic 01's "tens of ms" estimate is conservative; on NVMe SSDs we get sub-ms. The indexer can re-open the dataset on every run without worrying about startup cost.

### Manual procedure log

```
$ cargo run --bin spike-lance -- all

{"step":"schema","fields":4}
  field: id  type: Utf8
  field: title  type: Utf8
  field: body  type: Utf8
  field: embedding  type: FixedSizeList(Field { data_type: Float32, nullable: true }, 384)
{"step":"init","table":"artifacts","rows":6,"dim":384}

--- bm25 'borrow' ---
{"step":"bm25","q":"borrow","rows":1,"latency_ms":10.38}
  → Visualizing the Borrow Checker

--- vec 'borrow checker' ---
{"step":"vec","q":"borrow checker","rows":5,"latency_ms":7.06}
  → INC-0315 Summary
  → A Field Guide to Rust Errors
  → Server-Sent Events in axum
  → Everything HTML Can Do
  → Visualizing the Borrow Checker

--- hybrid 'SSE reconnect' ---
{"step":"hybrid","q":"SSE reconnect","rows":5,"latency_ms":13.84}
  → The Cost of Abstraction
  → Server-Sent Events in axum
  → Everything HTML Can Do
  → Visualizing the Borrow Checker
  → INC-0315 Summary

--- cold-open ---
{"step":"cold-open","latency_ms":0.68}

--- evolve ---
{"step":"evolve","add_columns":"OK","bug_3136":"not observed in this version"}

--- on-disk size ---
  ~/project/kb/spikes/lance/dataset
  ~/project/kb/spikes/lance/dataset/artifacts.lance
  ~/project/kb/spikes/lance/dataset/artifacts.lance/_versions
  ~/project/kb/spikes/lance/dataset/artifacts.lance/_indices
  ~/project/kb/spikes/lance/dataset/artifacts.lance/_indices/94cf05ee-722d-4f7d-a9d6-c22b904c4825
  ~/project/kb/spikes/lance/dataset/artifacts.lance/_transactions
  ~/project/kb/spikes/lance/dataset/artifacts.lance/data
  dataset size: 20535 bytes

OPERATOR CHECKLIST:
[x] init step printed rows=6, dim=384
[x] bm25 query returned matches for 'borrow' (Visualizing the Borrow Checker)
[x] vec query returned ranked nearest (5 rows; exact-match ranks top-1 in integration test)
[x] hybrid query returned 5 mixed-source results
[x] cold-open latency 0.68ms — far under <100ms target
[x] evolve step printed "OK" — bug #3136 NOT observed in lance 4.0.0
[x] dataset size 20KB (6 rows + FTS + IVF-PQ) — small as expected
[x] on-disk layout has _versions/, _indices/, _transactions/, data/
```

### What I would NOT carry forward
- The toy `xorshift64`-seeded embedding — replaced by real fastembed output (per spike-fastembed) in production. The spike's deterministic-by-hash embedding is only for testing pipeline correctness; the actual semantic vectors must come from bge-small-en-v1.5 or equivalent.
- The hand-curated 6-document seed list — production seeds from the file watcher + walker.
- The `Doc` struct's narrow shape — production needs all the facet booleans (`has_svg`, `has_details`, `has_script`, `has_form`, etc.) listed in topic 01's schema, plus `path`, `prompt_size_bytes`, `kb_category`, `headings`, `code`, `prompt`, plus `pages: Vec<Page>` for multi-file artifacts.
- The `init` function's "remove dataset first" pattern — production NEVER deletes the user's index; it does incremental updates only.
- The verbose `print_titles` debugging output — production logs via tracing, not eprintln.

---

## v0.7 H2 · ONNX Runtime in Docker · 2026-05-14

Not a formal spike — a findings note from wiring embeddings into the
Docker image (Track H2), the spike-fastembed deferral finally closed.

### Surprised

- **`ort-load-dynamic` downloads nothing at build time.** fastembed's
  `ort-load-dynamic` feature means `ort-sys`'s build script skips both
  linking *and* downloading — the `.so` must be supplied at runtime via
  `ORT_DYLIB_PATH`. So the Docker builder has to source
  `libonnxruntime.so` itself.
- **The exact ONNX Runtime version is recoverable from `ort-sys`.**
  `ort-sys`'s `build/download/dist.txt` lists the pyke CDN URLs keyed
  by the ONNX Runtime version it targets — `ort-sys 2.0.0-rc.12` →
  `ms@1.24.2`. That maps 1:1 to the official Microsoft release
  `onnxruntime-linux-x64-1.24.2.tgz`, which IS ABI-compatible (pyke
  repackages MS builds). The Dockerfile pins `ONNXRUNTIME_VERSION` to
  it; bump both together when `ort` moves.
- **The pyke `none` linux tarball ships a `.a`, not a `.so`.**
  `cdn.pyke.io/.../x86_64-unknown-linux-gnu.tar.lzma2` contains only
  `libonnxruntime.a` (static) — useless for `load-dynamic`. The
  official MS `.tgz` is the right source for the shared lib.
- **The model-prefetch step needs ORT loaded.** `kb model download`
  constructs an `Embedder`, which opens an ORT session — so the
  builder must have `ORT_DYLIB_PATH` set before that `RUN`, not just
  the runtime stage.

### Worked as expected

- A 384-MB → ~404-MB image with ONNX Runtime + the bge-small model
  baked in; `docker run` + index the canon corpus + a semantic query
  returns ranked hits with no ORT errors.

## S-milestone · scale to 50k docs · 2026-05-19

The largest kb at HEAD has 288 docs; the SPA fetched the whole list
on every gallery load and filtered/sorted client-side, the
folders/tags facet endpoints capped at 2000 docs scanned, and
AtlasView painted one SVG `<circle>` per dot. None of this broke at
288 docs but the architecture has a hard ceiling around 5–10k
artifacts.

Eight green-CI commits inverted the data flow without changing the
visible UI for kbs under that ceiling:

| Phase | What landed |
|---|---|
| S1 | `kb_core::docs_query` — pure filter/sort/paginate/aggregate, mirrors `web/src/routes/gallery.tsx` + `web/src/lib/sort.ts` |
| S2 | `/api/kb/<kb>/docs` paginated envelope `{docs,total,offset,limit,has_more}` + Link rel=next; folder/tags/caps/since/sort/projection params; `?legacy=1`-style shim (envelope only when `offset=` is present) |
| S3 | `/folders` + `/tags` walk full corpus via `aggregate_*`; 2000-cap gone |
| S4 | `useDocs` hook (debounced, URL-driven, paginated) behind `localStorage.kbScaleMode` |
| S5 | `@tanstack/react-virtual`-backed VirtualGrid/VirtualList; detail siblings → slim projection |
| S6 | AtlasView → Canvas2D (DPR-aware, hit-testing via logical-coord nearest-neighbour) |
| S7 | `group=folder` axis on the server (folder ASC prepended to primary sort); GroupControl restored under virtual-scroll |
| S8 | `kb synth --docs N` deterministic corpus generator; scale-mode default; flag removed |

### Surprised

- **Lance has no `order_by`.** `lancedb 0.27.2`'s `Query` API exposes
  `select`, `where`, `limit`, `nearest_to`, but no ordering. The
  initial bug fix that prompted this milestone (a 288-doc kb where
  `?folder=pm` rendered empty) hinged on `list_docs_inner` sorting
  by `indexed_at_unix DESC` *after* the lance scan — an in-memory
  Vec sort over the slim projection, which is cheap up to ~50k rows.
- **A 2000-doc cap on facets hides itself.** The old folders/tags
  routes scanned the first 2000 rows; a kb with 5000 docs would
  silently miss 60 % of its folders on the LeftRail. The lack of a
  visible failure mode meant the cap survived from v0.6 to S3 without
  question.
- **`useWindowVirtualizer` is the right primitive for an existing
  document-scrolled SPA.** `useVirtualizer` (the parent-scroll
  variant) forces an inner overflow container, which would change
  scrollbar / wheel ergonomics for the whole gallery. Window
  virtualization with `scrollMargin = parent.offsetTop` preserved
  every existing keystroke / scroll-restore detail.
- **Canvas2D handles 50k dots without WebGL.** Batched `ctx.fill()`
  + `ctx.filter = "blur(14px)"` for the density layer paint a 50k-dot
  scene in <5 ms per frame on a 2019 laptop. The SVG version with
  the same dot count regressed to single-digit FPS during pan/zoom.

### Reproducing at 50k

```bash
# Generate the corpus (deterministic — same seed = same bytes):
kb synth --docs 50000 --out /tmp/synth-50k

# Point a daemon at it via kb.toml:
#   [kb.synth]
#   path = "/tmp/synth-50k"

# Hot loop on the SPA's gallery + filter axes:
curl -s 'localhost:4000/api/kb/synth/docs?folder=ideas&offset=0&limit=200' | jq '.total'
curl -s 'localhost:4000/api/kb/synth/folders' | jq '.folders | length'
curl -s 'localhost:4000/api/kb/synth/tags' | jq 'length'
```

Targets (the plan's budget; measurement is a follow-up):

| Route | Budget |
|---|---|
| `/docs?folder=X&limit=200` | <100 ms p99 |
| `/folders` | <100 ms p99 |
| `/tags` (top 50) | <100 ms p99 |
| Gallery initial render | <500 ms to interactive |

A proper `cargo test --features perf` harness is deferred — the synth
corpus is the prerequisite, and it now exists. Wire up
`tests/perf/scale.rs` when the next milestone wants benchmark
regression guards.

## search-perf · 2026-06-05

Two search-latency wins on the live client daemon (`platform` kb, 518
docs, 1024-dim bge-large on CPU), **found and verified with the new
opt-in detailed metrics** (`GET /api/metrics` + `kb metrics`, gated by
`[server] metrics = true` — TM-track, commit `e80e40b`). The metrics
decomposed `/api/search` into embed / bm25 / vector / hybrid stages plus
per-`StorageKind` storage-actor timing, which is what isolated the real
bottleneck below (it was NOT what you'd guess).

### Measurements

`platform` kb, warm (steady-state) searches, client-side wall time:

| Surface | Before | After | Win |
|---|---|---|---|
| keyword search (BM25) | 535–726 ms | **5 ms** | ~100× |
| hybrid / semantic search | 680–890 ms | **~120 ms** | ~6–7× |
| `admin` storage-op p50 (the `ensure_*_index` calls) | 500 ms | **1 ms** | — |
| repeat hybrid query (cache hit) | 8 ms | 8 ms | (already optimal) |
| gallery `/docs` (40) · artifact open | 1–2 ms · 7–10 ms | unchanged | (already fast) |

Navigation was never the problem; search was — and not for the obvious
reason.

### Fix 1 — gate FTS/vector index rebuilds (`perf(kb-core)`, `158f252`)

**Root cause:** `Storage::ensure_fts_index` (5 FTS columns) and
`ensure_vector_index` (IVF-PQ) called lancedb `create_index`, whose
default `replace=true` is a **full inverted-index re-train**, not a
skip-if-exists. The scalar BTree indexes already had a `scalar_indexes_
ready` gate; FTS + vector did not — so the `"Index already exists"`
branch was dead and **every** `/api/search` + `/api/memory/recall`
rebuilt all six indexes (~500 ms on 518 docs). A keyword search whose
BM25 query is ~10 ms took ~535 ms; embedding was cache-hit `0 ms` and
still the whole request was slow. The production indexer never ensured
after upsert and compaction is startup-only, so the per-query rebuild
was *also* the only thing keeping new docs searchable.

**Fix:** `fts_needs_build` / `vector_needs_build` dirty flags on
`Storage` (init `true`), set by every content mutation (`upsert_docs` /
`delete_by_id` / `delete_by_path` / `delete_all_rows` → both;
`clear_embeddings` → vector). `ensure_*` fast-returns when clean and
rebuilds only when dirty. `update_atlas` deliberately does NOT dirty
them (atlas coords don't touch FTS columns or the embedding).
**Freshness is preserved exactly:** a mutation re-dirties → the next
search rebuilds → the rebuild incorporates the new rows — the same
result the per-query rebuild produced, now paid once per change instead
of once per query. No reliance on lance scan-fallback.

### Fix 2 — persist the query-embed cache across restarts (`perf(server)`, `550120e`)

After Fix 1, the bge-large embed (~50–140 ms) is the remaining cost of a
*novel* vector-mode query. The daemon-wide query-embed LRU already made
*repeats* free (`cache_hit` → `embed_ms 0`), but it was in-memory only
and wiped on every restart — including the CE-track **in-process config
restart** (every `PUT /api/config`). So hot queries re-paid the embed
after each restart.

**Fix:** persist the most-recent 256 entries to
`<state>/query-embed-cache.json` on shutdown (atomic via
`meta_edit::write_atomic`; snapshot-under-lock then write outside it),
reload in `KbHandles::new`. Capacity 256 → 1024. The model name is
re-interned via `embed::model_info` on load (unknown-model entries
dropped) — **safety-critical** because two models can share a dim
(bge-base + jina are both 768), so a vector served under the wrong model
key is silent garbage. Verified end-to-end: a novel query
(`cache_hit=false`), then a daemon restart, then the SAME query →
`cache_hit=true, embed_ms=0` on the first post-restart request.

The design was **deep-reviewed by a parallel Opus agent before
implementation** (the repo's `/deep-review`), which caught a CRITICAL
save-site bug (the cache `Arc` is moved into `build_router` before the
intended save line — now cloned at the hoist block), the wrong
atomic-write primitive, and that a warm-across-restart e2e needs
onnxruntime (so the round-trip is covered by pure unit tests instead).

### Architecture decision updates

- **`create_index` is `replace=true` by default** — treat any
  `ensure_*_index` as a full re-train and gate it behind a
  dirtied-on-mutation flag, never call it per query. (Generalises the
  pre-existing `scalar_indexes_ready` gate to FTS + vector.)
- **Index freshness via dirty-flag, not scan-fallback.** Rebuild-on-
  dirty is correct regardless of whether lance scan-fallbacks unindexed
  fragments, and pays the cost only when data actually changed.
- **The model name is load-bearing in the embed cache key** — both
  in-memory and persisted. Same-dim different-model collision is silent
  (no dim error), so persistence must store + re-intern the model.

### What I would NOT carry forward / caveats

- A genuinely-novel query's first embed is irreducible without a
  smaller/faster query model (which would break vector compatibility
  with the bge-large doc embeddings). Persistence only spares repeats —
  now including repeats across restarts.
- The dirty-flag is correct only if EVERY FTS/vector-affecting mutation
  calls `mark_search_indexes_dirty`. A new lance write path that skips
  it would silently serve stale search results. The mutation set is
  small + enumerated; a new one must opt in.
- Persist cap (256) < in-memory capacity (1024): the high end of a full
  cache is intentionally not persisted, so "warm across restart" is the
  256 most-recently-used queries, not all 1024.


## v0.18 — Reading Lists (RL-track)

What surprised us shipping the bookmarks → reading-lists replacement:

- **Derive, don't store.** Read state computed per response from the
  RP-track capture (override > section dwell > scroll completion) cost
  one `(DocSummary, ReadingSummary)` join per distinct artifact and
  removed an entire class of staleness bugs. The only stored state is
  the manual override. Corollary discovered in e2e: liveness rides
  `history.recorded`, which is INSERT-only — shared fixtures that
  already had a visit within the 30-min gap never re-fire it. Specs now
  mint fresh artifacts.
- **`lance word_count` already existed** (v0.6 I1) — the planned
  "additive column for time estimates" was a no-op; only per-SECTION
  words (computed from the anchor's heading range, refreshed by the
  ListAnchorHook) were new.
- **Row-persisted anchor staleness beats a sidecar.** Entries are
  sqlite rows, so `anchor_stale` lives on the row and a FRESH indexer
  detects 1→0 transitions after a restart — pinned by killing indexer A
  mid-lifecycle in the test.
- **Table-wide entry PKs + portable exports** = cross-list imports must
  REMINT ids (the 503 the first round-trip e2e caught); same-list
  replace preserves ids + `created_at`.
- **Never commit while a backgrounded `just types` is mid-run** — the
  recipe `rm -rf`s the generated dir first; a commit between the wipe
  and the kb-server export half captured a tree missing ~60 bindings
  (caught immediately by the SPA build, fixed by amend).
- The events schema's `per_type` match had four kinds in the enum but
  no arm since S5 (`atlas.recluster.*`, `maintenance.compact.*`) — a
  latent `unreachable!()` panic on schema introspection, fixed in
  passing when registering the eight `list.*` kinds.

## v0.19 — Parallel cross-kb fan-out (FF-track)

Every federated ("scope=all"/fleet) read handler was a serial
`for (name, ctx) in state.kbs` await loop whose latency grew linearly with
corpus count (the operator runs ~18). FF-A..FF-E converted them all to bounded
concurrent fan-out via `routes::buffered_join(futs, FANOUT_CAP=8)` (`buffered`,
submission-ordered — see invariant #28).

**Bench (FF-F).** `crates/kb-server/tests/fanout_bench.rs` (ignored) boots 16
independent per-kb corpora (each its own storage actor) and times the fleet
endpoints warm. Because `buffered(cap.max(1))` with `FANOUT_CAP=1` runs corpora
strictly one-at-a-time in submission order — the SAME code path with concurrency
removed — comparing a `=1` rebuild against `=8` isolates the fan-out effect
exactly (same fold, same sort, only concurrency differs):

| endpoint | serial p50 (cap=1) | parallel p50 (cap=8) | speedup |
|---|---|---|---|
| `/api/search?scope=all` (kw) | 231.4 ms | 126.0 ms | 1.8× |
| `/api/notes` | 98.4 ms | 59.7 ms | 1.6× |
| `/api/stats` | 1.61 ms | 0.71 ms | 2.3× |
| `/api/kbs` | 1.03 ms | 0.50 ms | 2.1× |
| `/api/sessions` | 0.47 ms | 0.30 ms | 1.6× |
| `/api/lists` | 0.39 ms | 0.24 ms | 1.6× |

Consistent 1.6–2.3× at p50 across every federated surface. The parallel run is
capped at 8 concurrent over 16 corpora, so the win is *understated* here — it
grows with corpus count and `FANOUT_CAP` (serial ≈ Σ per-corpus latency →
parallel ≈ max per-corpus + small overhead). The two heavy paths (search, notes)
see the largest absolute reductions. Re-run:
`cargo test --profile fast -p kb-server --test fanout_bench -- --ignored --nocapture`.

## v0.29 — the web UI frontier, Wave 3 (the six moonshots)

Wave 3 of `docs/research/kb-web-ui-frontier-synthesis-2026-07.html`. A 5-agent
recon pass mapped all six moonshots against HEAD before any code was written;
**every verdict came back `build-with-amendments` — none was refused** — and the
amendments below are what the recon changed about the plan.

### The census gate, ruled

The synthesis makes map-home-vs-replay an *evidence-gated* promotion (standing
rule 3): map-home replaces the session-replay reader as the flagship only on
**observed** atlas usage. `web/src/lib/census.ts` is local-only and had shipped
days earlier, so it held essentially nothing — **the gate could not have
fired**, and the named default stood. Map-home is therefore fully built but
ships `Prefs.home = "grid"`, reachable only via an explicit `?shell=map` or a
Settings toggle, with `NAV_ITEMS` untouched and a Playwright regression pinning
a bare `/` to the grid. A prominent placement would manufacture the very
evidence the census exists to measure.

### Three defects the recon found (shipped bugs, not new features)

1. **The atlas was not rendering your corpus.** `gallery.tsx` passes pageSize
   200 to `useDocs` and the atlas branch never calls `loadMore`, so `platform`
   (1089 docs) drew 200 dots while the status line claimed "200 artifacts".
   Raising the page size is a dead end (envelope mode caps at 500;
   `?projection=atlas` is served uncached) → `GET /atlas/points`, generation-memoised.
2. **The atlas canvas transform was non-uniform** (`ax = cssW*dpr/W`,
   `ay = cssH*dpr/H` against a fixed logical 600×360), so any non-5:3 container
   drew every `ctx.arc` dot as an ELLIPSE while `findHit` still tested a circular
   radius — the map both looked wrong and hit-tested elsewhere. Already live at
   ≤860px. Fixed by `lib/atlasFit.ts` before the map-home shell made it universal.
3. **Suffix-only origin checks are not multi-pane safe.** `isArtifactOrigin`
   passes for *every* artifact iframe, so a second pane's `kb:scroll`/`kb:reading`
   beacons would have been accepted by pane 1's handler and POSTed against the
   wrong artifact (#8/#19). Closed by `isOriginOfArtifact` *before* a second
   iframe existed.

### The deciding fact for the flagship

Per-event timestamps are **not** in sqlite — `session_decisions`/`_commits`/
`_research` carry only `seq`, `session_files` carries neither. But they **are**
recoverable from the capture's byte-identical `<pre>` via the shipped
`recover_jsonl_from_capture`: measured on a real 5835-record transcript, 100% of
`user` (1349) and `assistant` (2120) records carry an ISO `timestamp`, and
`parse_session_activity` already reads that field and throws all but the max
into `ended_at`. **So the playhead needed ZERO schema change** — the replay
timeline is a pure derivation memoised in a process-local LRU, never a
StorageMsg, never a generation bump.

Two consequences worth keeping:
- The cache key carries the **scrub posture** as a fourth component. Without it
  a loopback request could warm the cache with an unredacted timeline that a
  later non-loopback request would then be served, silently defeating the
  fail-closed rule (#4).
- `ArtifactPane` **cannot** back the replay stage: it calls `recordOpen`
  unconditionally on mount, so scrubbing N beats would insert history rows for
  artifacts nobody opened. Replay renders its own minimal iframe instead.

### Scope amendments accepted

- **Replay** descoped from "watch the artifact come into being" to "highlight
  what was being read/edited at this beat". Byte-level reconstruction is not
  buildable: `artifact_snapshots` only append on content change, are capped,
  exclude memory-session artifacts, and no route serves bytes at a version ref.
  Highlight is SECTION-granular (line ranges exist for only ~21% of `Read`
  calls), labelled "detected", and rides the existing `kb:scroll-to-id`+`flash`
  channel with **zero new iframe code**.
- **Split panes** descoped from an n-pane workspace grammar to a two-pane
  artifact compare mode — four singletons block the general case (see #30's
  v0.29 amendment).
- **Procrustes** needs no dependency: the 2-D orthogonal case has a closed form
  using only `+ − × ÷ sqrt`. Taking `cosθ = a/r` and `sinθ = b/r` directly (never
  `atan2` then `cos`/`sin`) is mandatory — transcendentals are
  libm-implementation-defined and would reintroduce the v0.7.1 C2 cross-libc
  determinism bug that Box-Muller → Irwin-Hall fixed.

### Honesty carried on the wire

- The Procrustes **residual can never be zero**: `normalise_to_unit` min-max-scales
  x and y *independently*, so every stored frame is already anisotropically
  stretched and no similarity transform can undo it. It is reported, not hidden.
- **Cluster ids renumber between frames** — `kmeans_lloyd` seeds by array
  position and reseeds empty clusters randomly — so the time-lapse remaps colours
  per frame (`lib/clusterRemap.ts`, deterministic greedy nearest-centroid under a
  total order). Without it, playback strobes meaninglessly.
- **Frames start empty.** Nothing retains a past layout or a past embedding, so
  true backfill is impossible; the empty state says frames are recorded from now
  on rather than rendering a blank chart.
- The comment lane counts comments **raised**, as recorded by this daemon —
  kb-comments/1 has no `resolved_at`, and walking every `.review/*.json` per
  request is the cost shape `inbox.rs` already refuses.

### Storage ledger (the one-container-primitive rule, per moonshot)

| Moonshot | New durable noun |
|---|---|
| Session-replay reader | **None** — pure derivation + a process-local LRU |
| Map-home shell | **None** — one generation-memoised read |
| Dual-field atlas | One **sidecar** (`atlas/operator.canvas`), boards precedent |
| Loci tours | **None** — a tour IS a reading list (ordered `position`, `?list=&entry=`, QueueBar) |
| Corpus time-lapse | **V0028** `atlas_snapshots` + `atlas_snapshot_points` (sqlite, the V0027 lane) |
| Two-pane reader + registers | **None** — `?pane2=` URL grammar + a browser-local 26-slot blob |
| Reflection canvas + scenes | **None** — a scene is an existing `SavedQuery` |

A scene is a QUERY, not a collection: materialising a brush as a list would
freeze a result set whose whole point is to re-derive.

### Operational note (multi-agent builds on one box)

Five parallel worktree builders, each with its own cold `target/`, drove this
8-core host to load 173 and exhausted 32 GB of swap; that batch took 4.5 h.
Four-wide, mixed 2-Rust/2-SPA, runs at load ~25–40 and is faster in wall-clock.
`AtlasView.tsx` was the schedule's critical path — five units mutate it, so at
most one atlas-SPA unit per batch, or the merge is unresolvable.

## v0.34 — kb-users/1: identity as attribution

The operator asked for a team inside one daemon: separate reading state,
comments, and API/CLI usage per person, all mutually visible. That reverses
half of the 2026-07-06 "one daemon, one operator" ruling — so the milestone
started by naming exactly which half.

### The line that made the scope decidable

**Identity is attribution, not authorization.** kb learns *who* acted; it
never decides *whether* they may act. Everything that made the original
ruling a refusal survives: no passwords or sessions in the daemon (the edge
owns authn), no roles, no read-only tokens, no ACLs, no visibility tiers.
One trust tier, N names on the ledger. Without that sentence every question
("should a teammate see memory?", "can they DELETE /api/kb?") reopens the
whole security model; with it, each answers itself.

### Authelia stays the authenticator — the plumbing was already there

The research (verified, 16 findings) turned up two facts that decided the
architecture rather than merely supporting it:

- **Traefik's forwardAuth deletes then replaces** every header listed in
  `authResponseHeaders`, so a browser-forged `Remote-User` cannot survive
  the hop — *provided the header is listed*. An unlisted identity header is
  trusted forgery. That moved "list it" from advice to a ship gate.
- **Authelia sets no `Remote-*` for `client_credentials`** — machine clients
  have no username to forward. So proxy headers alone can never attribute a
  bot, which is what forced per-user tokens into v1 rather than "later", and
  what settled the ladder order: **token beats header** (explicit beats
  ambient). `X-Kb-Token` exists because the edge's `kb-inject-bearer`
  overwrites `Authorization` — an agent behind the proxy has nowhere else to
  put its own credential.

### What a two-seat adversarial panel caught that the draft missed

Both seats returned "revise" on a design that looked finished. The five
load-bearing catches, all folded before any code:

1. The draft fixed the 30-minute open-visit dedup but left **search history
   dedup** keyed on query alone — "track everything separately" would have
   shipped false.
2. The reading rollup's override overlay reads `list_entries.read_override`
   at a *different call site* than `derive_read_state`; freezing the column
   without switching that overlay would have left `?read=` silently
   operator-global.
3. The SPA's edit gate was client-only. With N co-operators that is a
   footgun, not a UI detail — hence the server-enforced owner-only rule.
4. `auth_bearer` returns early on loopback **before reading Authorization**;
   bolting identity on "after auth succeeds" would have made every loopback
   agent attribute as the operator regardless of its own token.
5. Nothing in the draft said memory and sessions stay shared. The original
   ruling's objection was the privacy leak, not just read-state pollution —
   so the non-goal now says, in words, what a teammate can see and do.

### Two process lessons

**Harvest with explicit pathspecs.** A `git add -A` inside a build worktree
swept a concurrent session's uncommitted kb-code v3.1 work into the X1
commit. It was extracted in a follow-up (`548d66f`) rather than by rewriting
pushed history; every later phase staged named paths only.

**A "revise" verdict is the reviewer working, not the lane failing.** All
four builder lanes came back green-on-tests and revise-on-review; every
blocking item was real (a batch-path bypass of the owner gate, a
`KB_ALLOW_NO_AUTH` regression that would have 401'd browser users the moment
a token registry appeared, a hardcoded `"operator"` literal where the
configured name belongs). The orchestrator fixing those directly — rather
than round-tripping to the builder — is what kept the phase count at four.

## kb-code v4.0 — "The Review Room" (V4 tracks U/L/C/D/S/M/P)

One day, 2026-08-14→15: 20 phases, ~24 green-CI commits, four full
Playwright suite runs (final: 125 passed / 0 failed). Every phase built by
a grok-builder lane in its own worktree; the orchestrator (Fable) reviewed
every diff, ran gates serially on main, and committed. Recon (4 sonnet
readers) + design (3 Plan agents) preceded any code; all four operator
direction rulings took the recommended option.

### What shipped

Review comments became kb-comments-grade: review-scoped annotations pinned
to patchset blobs (V0023), lazy per-read carry-forward with the
snippet-guard ladder (an uncertain match is an honest orphan, never a
guessed line), verdicts with `verdict_ps` staleness, atomic batches, and
`GET /reviews/{id}/comments`. Suggestions are a full loop: CM6 editor →
live one-hunk preview (dogfooding the diff renderer) → stored on the
thread → loopback exact-match-splice apply (drift = structured 409, tree
untouched — deliberately NOT `git apply --3way`, which can leave conflict
markers from a one-click button). Branches gained remote enumeration,
origin/HEAD default resolution, and `?sort=suggested` with named terms
feeding the new ~branches landing page. The diff surface: hand-rolled
side-by-side (no @codemirror/merge — threads are grid rows and git's line
numbers stay the anchor coordinate system), server-span syntax highlight
behind a per-line integrity guard, and the full-page keyboard-driven
review route. Chrome: light/system theme wired (the CSS existed dead for
months), token ladders, 40+ icons, TopBar IA + mobile nav sheet, review
sheet + coarse-pointer comment pills. CLI parity incl. `annotate watch`.

### Findings worth keeping

1. **SSE ring-buffer replay is a test trap.** `GET /api/events` replays
   from cursor 0 on connect; four fresh SSE tests counted their own
   setup's replayed events and one "handler bug" (verdict no-op emitting)
   was actually the test forgetting to drain. `drain_sse_backlog` helpers
   now live in both server harnesses; the CLI watch verb is replay-proof
   by construction (seed-then-diff seen-set).
2. **First-execution spec fragility is a predictable tax.** Builders wrote
   Playwright specs they could never run; first suite executions surfaced
   four spec bugs (absolute counts over a SHARED fixture repo, exact
   counts on a deliberately-fuzzy filter, a resolved thread expected to
   orphan under the open-only default, and LIVE `.first()` locators
   re-resolving after later inserts). All four repairs asserted the
   product contract more precisely; zero product defects.
3. **Worker wall-clocks vs this disk-bound box.** Three server lanes hit
   their ~60-min budgets mid-link/mid-verification with the code complete;
   the relay-verifies-then-orchestrator-gates pattern absorbed all three.
   Brief server lanes with "prefer finishing code over long verification,
   report what wasn't run".
4. **Harvest with explicit pathspecs — again.** `git commit` sweeps the
   whole index: with two lanes' patches applied (one still gating), the
   M1 commit initially absorbed P1's staged tracked files (untracked
   `mod`-referenced files left behind → a broken commit). Caught before
   push, split via soft-reset. The v0.34 lesson ("harvest with explicit
   pathspecs") now extends: never `git apply` a second lane onto main
   while another lane's harvest is uncommitted.
5. **A latent light-theme bug hid in plain sight**: graph components
   referenced `--amber`/`--bg-raised` — tokens that never existed — so
   dark fallbacks painted in both themes; the U4 sweep's grep found 14
   sites (5 beyond the design audit).

## v0.37 — "SPA Shine": both SPAs, one design pass (SH tracks A/B/C/D/I)

One milestone, both SPAs, zero Rust: a 9-agent exploration (6 structured
audits + baseline gates + 74 before-screenshots, both themes × desktop +
390px) fed a Fable design synthesis, then 9 Sonnet build units in two
waves + three follow-ups landed 14 commits (3cd27ab5..2ea27f3d). Suites
grew web 1098→1115, web-code 891→909; gates: ci-e2e 264/264, ci-code-e2e
125/125; 13 defect routes re-screenshotted and eyeball-verified after.

What shipped, compressed: measured-AA color work (light semantic palette
port, --accent-fill via color-mix so the Settings accent picker keeps
working, --ink-dim/--ink-faint split, white-on-red fills); the /memory
mobile P0 (desktop rail painted over 70% of the viewport — now
card-stacked, CSS-only via grid-template-areas over the existing cell
classes); the icon-discipline program (10+2 drawn glyphs, ~40 emoji/
dingbat/typed-glyph call sites onto the Icon grammar with aria preserved,
a cross-SPA parity golden that fs-reads BOTH icons.tsx and caught a real
drift during development); mobile-shell repairs (iOS 16px inputs, edge-
fade + scroll-snap overflow affordances on every clipped toolbar/tab row
in both SPAs, status-pill/FAB content overlaps, shared useBodyScrollLock,
TocSpy hidden at ≤860px WITH its replacement — a Topics section in the
reader sheet riding the same kb:toc relay and the ?sec=/?pane2= params);
kb-code reading modes (persisted wrap + font-size via CM6 compartments);
and the gallery-lobby redesign (tagColor-stable slim census bar,
clamped highlight cards, visit-gated auto-collapse).

Findings worth keeping:

1. **The two token systems drift measurably when only one gets audited.**
   web-code's F5 pass darkened light-theme --warn/--green/--red/--blue
   with documented arithmetic; web/ — the ORIGIN of that palette — still
   shipped the failing values months later, and web-code had silently
   dropped web's --ok/--danger aliases so review file-stats rendered
   their never-themed hex fallbacks. Parity is now partially pinned (the
   icon golden); token parity still relies on discipline.
2. **Screenshot agents find what greps cannot.** The tofu U+FF0B plus,
   the rail-over-content P0, the TOC covering the H1, the flat dark
   scrim, and the pane-focus accent line painting on a single pane were
   all invisible to static audits and obvious in one 390px screenshot.
   The before/after re-shoot also caught the one regression this
   milestone itself introduced (lobby card footers clipped by the
   VirtualGrid-coupled fixed card height) before the tag.
3. **Root-cause beats retune.** Three "cosmetic" defects were state bugs:
   the orphaned tree focus ring (focusedIndex=0 default styling row 0
   with no real focus — fixed by :focus-within scoping), the stray
   accent rule (pane-1 is-focused with no pane 2), and "PROJECTS1
   session" (a head-meta row that was never display:flex).
4. **A text glyph WAS the accessible name.** The icon sweep's one real
   hazard: swapping ×/⚠/👍 for aria-hidden SVGs silently strips button
   names. Every swap carried aria-label/sr-only treatment, and exactly
   one e2e spec (drain's ^⚠ drain$ anchored regex) needed its anchor
   relaxed — everything else already selected by role/testid.
5. **ci-e2e could never run on this box.** The recipe's raw
   `playwright install --with-deps` exits 127 without apt-get;
   ci-code-e2e had carried the documented fallback since its
   introduction. The gap only surfaced because this milestone actually
   ran the full gate locally (84d5cdd8).

## v0.38 — "Connective Tissue": memory × sessions × code × artifacts (CT waves A–F)

The pairs kb already stored but never read back. A 14-agent synthesis
(Fable orchestration, Sonnet exploration, Kimi/Grok extra lenses) produced
a red-team-verified program artifact
([kb-connective-tissue-program-2026-08.html](research/kb-connective-tissue-program-2026-08.html),
id `258a4cc59162`), which the operator ratified in full; ~50 commits landed
it across six waves, every one Sonnet-built in an isolated worktree and
Fable-gated on main.

Three theses drove it: **(1) write–read asymmetry** — most of the value was
already on disk and simply never parsed back; **(2) label the join tier,
don't build an engine** — say "exact-id" vs "heuristic" vs "unverifiable"
rather than inventing resolution machinery; **(3) give the loop a
correction arc** — an agent that discovers a memory is wrong needed
something between silence and `kb forget`.

What shipped, compressed. **Wave A (read-backs):** U3 provenance metas now
parse back into four lance columns with a reverse "memories lifted from
this artifact" route; kb-code stopped dropping `memory_ids`/`kb` on the
why-panel; the recall→ledger hop gained a `kb-recall/1` machine marker
(free-text stays a permanent fallback) plus a three-way parse census;
`cat`/`get` finally produce research rows; touches carry Exact/Fuzzy
tiers. **Wave B (chains):** `kb why-memory` composes fact→session→
commits→files from three existing endpoints with zero new server surface;
`recalled-by` reverses the ledger; `kb memory expand` walks a highlight
back to its origin passage through kb-comments/1's own anchor ladder.
**Wave C (the correction arc):** `kb memory flag` (an ordinary comment —
no new storage), `kb remember --failed` + `warns`, injection-efficacy
`used`, kb-local `code_hints`/`drift_open`, `/kb-verify`, and `kb doctor
--hooks`. **Wave D (the one new surface):** `kb context`, a budgeted pack
that COMPOSES four existing reads. **Wave E:** daycard `?since=`,
attention-gap story beats, honest-staleness badges, the agent-hot/
human-cold drift meter, narrative reading lists, an echoes beliefs lane,
`kb-code review distill`. **Frontier:** `Kb-Memory:` commit trailers
(V0038 — the one item that earned a new table), era-resolved citations,
unlinked mentions, session residue, corpus SLOs (V0039), and a Memento
`?at=` resolver.

Findings worth keeping:

1. **"Surfaced, never scored" only holds if it's structural.** Six new
   display signals (`flagged`, `warns`, `used`, `drift_open`,
   `code_hints`, `session_residue`) ride recall/census wires. Each is
   computed strictly POST-rank and lives on a *different struct* from the
   scoring types — `session_residue` is on `DocResponse` but never on
   `DocRow`/`DocSummary`, so `docs_query::cmp_rows` structurally cannot
   see it. Each is pinned by a byte-identical-decomposition test. A
   comment saying "don't score this" would not have survived six
   independent builders.
2. **The acceptance criterion earned its keep — by being partially
   refused.** Wave D shipped only on condition that its three consumers
   rewire or it reverts. Two did. `/kb-weekly` did not, correctly: it
   makes exactly one call on a *temporal* axis, and a window is not a
   query. The criterion existed to stop a fifth assembler shipping beside
   surviving hand-chains; no such hand-chain survived, so the gate was
   met. Writing the refusal into the skill file keeps it from being
   re-litigated.
3. **A ratified design can still be wrong in one detail.** D1's first
   shape emitted the turn-1 scent *instead of* the recall block. R0/R3
   governs EPISODIC material; memories have been pushed every turn since
   v0.9. Replacing them would have cost the agent its memory titles on
   exactly the turn a task gets framed. Ruling: the scent is ADDITIVE —
   two tests rewritten, three added, including the scent-only case the
   replacing shape had silently lost.
4. **Registry absence needs the same ceremony as registry presence.**
   `memory_commits` is artifact-id-keyed and joins all three id-lifecycle
   registries; `slo_snapshots` is kb-keyed and deliberately joins NONE —
   letting a doc delete rewrite a past corpus reading would falsify the
   log's whole purpose. Both facts are argued in their migration headers
   and pinned by tests, because an unexplained omission is
   indistinguishable from the invariant-#2 mistake it resembles.
5. **`git apply --3way` is atomic, and lies about it.** One file failing
   with "does not match index" rolls the WHOLE patch back while still
   printing per-file "Applied cleanly" for the rest. Cost one silent
   no-op before the habit stuck: sentinel-grep every file after every
   apply. (A "with conflicts" apply, by contrast, does land.)
6. **Generated artifacts are settled by regenerating, never merging.**
   `docs/api-routes.md` conflicted twice between waves; both times the
   answer was `just api-docs`. The drift guards then caught two real gaps
   the harvests had missed — including one CT-B2 had left on main weeks
   earlier.

## v0.39 — "The PR Room": PR-review workspace × navigation instrument (PRR tracks R/N/L/U/F/G)

The operator's two asks — "solargraph-grade navigation, deep-link
everything, be wild" and "my LLM PR reviews (standalone HTML in the
acme-shop corpus) should land IN kb-code, workable by human AND agent,
publishable to GitHub manually" — became one milestone: ~30 commits, every
build unit Sonnet-in-a-worktree, Fable designing/gating/harvesting, the
interface designed by a Fable fork against the operator's own report-pr.html
aesthetic. Ratification reshaped two things mid-flight: live LSP became the
opt-in **lip/1 provider lane** (one generic adapter, N language configs,
blob-guarded exact, computed-fresh-never-persisted) rather than an in-daemon
client, and "GitHub conversation in the Room" was promoted to MUST.

Keepable findings:

- **Builders correctly refuse unverifiable mid-build scope messages.** Two
  units treated a legitimate SendMessage scope extension as a possible
  prompt injection and declined it — the right reflex. The pattern that
  works: write the change into the spec file on disk, point the builder at
  it; it verifies mtime + internal consistency and proceeds. Authority
  lives in artifacts, not in messages.
- **Harvest science for concurrent same-file appends.** git's 3-way merge
  interleaves both-append conflicts MID-FRAGMENT when fragments share
  interior context (store.rs methods, clap enums) — blind union produces
  garbage that only the compiler catches. The reliable resolution is
  scope-aware anchored insertion: parse the patch's hunks, place each
  pure-addition fragment after a unique lead (or before a unique trail, or
  any valid position in the right scope for order-free fragments like match
  arms and #[test] fns), then let fmt+clippy+tests judge. Also two traps
  that each bit more than once: `git apply` inside a compound
  `cd <worktree> && …` runs against the worktree, and piping apply through
  `| head` eats the real exit code while the atomic rollback prints
  per-file success.
- **Smoke against the real workload finds what fixtures can't.** kb-lip
  passed 49 fixture tests, then the live ruby-lsp run against the real
  Rails app surfaced three ship-blockers in one afternoon: LocationLink[]
  replies silently dropped, no current_dir on the child (Bundler
  detection), and "empty" hovers that were really indexing-blindness —
  fixed as an honest `refused:"indexing"` gate, plus the discovery that
  ruby-lsp only implements LSP 3.17 PULL diagnostics (L5 added the pull
  path). Post-fix: schema-aware rails-addon hover against production code.
- **Two builders typing the same wire concurrently reconcile to the
  stricter set.** U2 and U3 both typed kbc-findings/1; harvest kept the
  server-matched strict types canonical with the other unit's names as
  aliases and ONE fetch/hook pair — tsc drove the whole reconciliation.
- **The shared-CARGO_TARGET_DIR economics held at 4–5 builders** on this
  IO-bound box: flock waits up to ~23min, load 48, swap 100% — slow but
  never wrong (CARGO_INCREMENTAL=0 remains mandatory; wall-clock asserts
  in the measure lane are the only flake class).
- **Honest-orphan discipline generalizes to foreign anchors.** GitHub's
  own review comments re-anchor through the same exact→fuzzy→snippet
  ladder using each comment's diff_hunk last line as the snippet — a miss
  is an orphan card, and export refuses to post any line GitHub could
  render wrong. The one law ("an uncertain match is an honest orphan")
  now covers local comments, imported findings, AND both GitHub
  directions.

## v0.40 — "One Inbox": federated attention queue × LSP quick fixes × mobile mutations (S2 tracks A/B/C/D)

The operator's third ratification off the v0.39 recon maps ("after those
waves let's pick the other tasks — one inbox, mobile mutations, LSP code
actions, and lip providers — include also rust, keep out only the GitHub
push based") became four parallel units, gated the same way as v0.39:
Sonnet-in-a-worktree builders, Fable designing/gating/harvesting, spec of
record `/tmp/design-s2.md`. Wave A (server: mobile-mutations gate,
unified-inbox route, kb-lip's 6th endpoint, the provider-fleet files) ran
fully parallel; Wave B (CLI/SPA clients) forked after A landed; Wave C
closed with live smokes + this docs sweep. Shipped: `GET /api/inbox`
(three-lane federated queue — reviews, working-tree questions, a live kb
desk/comments pull, honest per-lane degrade, never a merged score);
`[review] remote_mutations` (default OFF, graduates five review-mutation
route families off loopback-only for a bearer caller, byte-identical 404
when off); `POST /lip/code-actions` + `POST /api/code-actions` (LSP quick
fixes, converted to suggestions via the EXISTING annotations/batch op —
no new mutation route); and the rust/typescript/python/go provider fleet
alongside `GET /api/repos`'s additive `intel_providers`.

Findings worth keeping:

- **Recon corrects scope before code, not after.** The design draft's
  first pass assumed annotation/question posting + replies + resolve
  needed graduating off loopback alongside the other five families; a
  recon pass against the actual router.rs (not the earlier design's
  assumption) found they were ALREADY bearer, narrowing "mobile
  mutations" to exactly the five stateful families before any builder
  wrote a line — absorbed straight into the spec file on disk rather than
  discovered mid-build and requiring a rewrite-and-reharvest.
- **A spend-limit kill mid-wave is a recoverable event, not a restart.**
  One builder hit its budget ceiling with a partial, otherwise-sound diff
  on disk. The recovery pattern: a fresh "finisher" agent briefed with
  the partial diff plus the unit's remaining scope, continuing from where
  the kill landed rather than redoing already-verified-good work from
  zero. Cheaper than a restart and now the default reflex for this
  failure class, distinct from the "builder declines a scope message"
  and "unit needs a redo" failure modes v0.39 already catalogued.
- **Three units on one file still resolves by straight union when the
  append-only discipline holds.** `router.rs`'s own module doc and route
  table are BOTH append-only conventions (the same "delimited from
  concurrent edits elsewhere in this file" comment style `main.rs`
  already used for PRR-L2/S2-B1). S2-A1 (the gated `review_remote`
  sub-router), S2-A2 (`unified_inbox` + one `/inbox` route line), and
  S2-B1 (`code_actions` + one route line) each touched the file
  independently; all three landed as a straight union — anchored
  doc-comment paragraphs appended in wave order, route lines placed
  beside their nearest literal sibling — with no manual reconciliation,
  because no two units ever edited the same line to begin with.
- **Pipe-swallows-exit-code struck again.** Same class v0.39 already
  named (`git apply --3way | head` eating a real non-zero exit while
  printing per-file "Applied cleanly" for the rest): a verification
  command piped through a truncating filter reported success on a run
  that had actually failed. The fix is unchanged and evidently needs
  re-learning per wave regardless: never pipe a command whose exit code
  gates the next step — capture it in a variable (or check
  `$PIPESTATUS`) before piping its output anywhere.

## v0.41 — "slate": a per-project blackboard for agents (SL0-SL6)

The operator's 2026-09-03 ask ("a way for the LLM to store information
and ideas, not memory — a blackboard specific to a project or room that
each LLM can open and read/write") became a seven-phase milestone: a
new `kb-slate/1` engine (kb-core), routes + a per-slate lock + SSE
(kb-server), CLI verbs + hook injection across six harnesses (kb-cli +
plugins/kb-memory), an SPA board (`/slates`), and — this phase (SL5) —
the tidy/distill skills, `docs/slate.md`, and a dispatcher bridge so
headless grokclaude/codexclaude/kimiclaude/ompclaude workers read and
write the same board a live session does. Design of record:
`docs/research/kb-slate-design-2026-09.html`. The 2026-09-04 board
amendment (drawings, emoticons, bigger/smaller text, grouping) added a
whole evidence-graded affordance map (§4) before any SPA code existed —
worth naming on its own, since it reversed the instinct to just add
fields for each ask and instead ran every affordance through "does this
survive serialization into a one-dimensional token stream" first.

Findings worth keeping:

- **A capture-adapter pattern generalizes past its first use case.**
  `trigger_kb_capture`/`kb-capture-*.sh` (sessions) and the new
  `trigger_kb_slate`/`kb-slate-harvest.sh` (slate) share the exact same
  shape — a fire-and-forget `bash <script> <arg>` spawned once a job's
  disk state is durable, gated on `KB_SESSIONS_DIR` + the fake-worker
  selector, script path overridable by env var for tests. Naming the
  pattern once (rather than re-deriving it) turned a "how should the
  dispatcher talk to kb" design question into "which of the five job
  exit paths call the existing pattern" — a much smaller question.
- **Every exit path is not two exit paths.** The design initially
  described "capture at the two normal exits" and, separately, worried
  whether reaped/closed jobs would look permanently live on the slate.
  Enumerating the actual control flow (`finish_ok_job`, `fail_job`,
  `App::reap`, `session_close`, and the session-round worker-failure
  branch) found three MORE terminal paths that bypass both trigger
  sites entirely — a take opened at spawn and never explicitly closed
  on any of those three would sit on the board looking live until its
  own liveness clock aged it out on its own, which is honest but slow
  and avoidable. A single `--abandoned` mode on the harvest script,
  called from all three, closes the gap without a second script.
- **A lookup-by-ref beats a stored id when the id would otherwise leak
  across a state boundary.** The take-on-spawn call and the
  harvest-close call are separated by an entire job run (minutes to
  hours) and, for the three bypass exits, never share a code path at
  all. Rather than adding a new `JobMeta` field to remember the take's
  post number, the take is posted with a `job:<ulid>` ref and the
  harvest script re-resolves the live post number fresh, every time,
  from `kb slate open --all --json`. No schema migration, no field two
  processes could disagree about the meaning of, and the lookup is
  cheap (a linear scan of the TAKE section, which the caps already keep
  small).
- **`done --abandoned` and `drop` are not interchangeable, despite
  reading that way in prose.** The design names both as acceptable for
  closing an abandoned take. Only `done` (with or without `--abandoned`)
  is exempt from the drop/edit live-author friction rule (`kind !=
  drop` and it carries no `supersedes`) — a `drop` on a take the
  daemon still considers "live" via the age-only liveness fallback
  (no session ever registered) would need `--anyway`, which a
  fire-and-forget bridge script should never pass blindly. `done` sidesteps
  the question entirely and was the correct choice once traced through
  the rules matrix rather than picked from the prose alone.
- **A skill's "report" can be its own actions.** The design's "tidy …
  reports as ordinary posts" reads, on a first pass, like it wants a
  distinct summary post. Re-reading against D17 ("erasure has a
  witness" — every drop/edit already carries an actor and a why, visible
  in the affected session's own next delta) makes it structural rather
  than stylistic: `/kb-slate-tidy` posts NOTHING beyond the drops,
  merges, and closes it actually makes. Inventing a summary post would
  have been the daemon effectively authoring content about its own
  content — the same "no in-daemon LLM, tidy is a skill" refusal the
  design already states, just easy to violate by accident in the
  reporting step rather than the decision step.
