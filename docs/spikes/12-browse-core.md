# 12 Browse core: PDH adapter and engine counters, decode cost, facet queries, the spill margin

Date: 2026-11-05

GPU: a 16 GB consumer GPU (driver 617.42). ComfyUI 0.42.1 (used only by S0.4, which the script starts on `127.0.0.1` after the owner says go).

## 1. Question

Plan 4b, Task 0. Four measurements that later tasks build on (Tasks 1, 5, 12 and 19):

- **S0.1, CORE-059:** can a process that isn't elevated read PDH's `GPU Adapter Memory` and `GPU Engine` counters for the discrete GPU, how fast is a collect, and what do the instance names look like? While a known load runs, how does the adapter's `Shared Usage` move, and how does the busiest engine type compare with NVML's utilisation?
- **S0.2, APP-038 (P4b-R12, P4b-R36):** what do decode, Lanczos3 resize and PNG encode cost in process with the `image` crate, do its limits refuse a decompression bomb cheaply, and do broken files fail without a panic? Does P4b-R12's in-process route stand, or does Task 12 need the helper process?
- **S0.3, APP-138, CORE-042 (P4b-R10, P4b-R23):** how long do `assets.query`'s page of 200 and its six disjunctive facet counts take, in one read transaction, at 10,000 and 50,000 assets? That sets `assets_query_facets_10000_ms` (twice the p95, rounded up to 10 ms) and the 50,000 figure.
- **S0.4, APP-132 (P4b-R19, P4b-R37):** is there a clean gap between the engine's `Shared Usage` above idle in runs that fit the card and in runs that spill, and so a spill margin to replace the starting 512 MiB?

## 2. Setup

| Item | Version |
|---|---|
| OS | Windows 11 Pro 10.0.26300 (26H2, UBR 9818) |
| GPU | a 16 GB consumer GPU (one discrete adapter), driver 617.42 |
| CPU and memory | a desktop CPU with 32 logical processors (so `min(4, cores/2)` = 4), 128 GB RAM |
| Rust | rustc / cargo 1.99.0, `stable-x86_64-pc-windows-msvc`, edition 2024 |
| Crates | `windows` =0.62.2, `image` =0.25.10 (`png`, `jpeg`, `webp`), `rusqlite` =0.40.2 (`bundled`, the workspace's version), `serde_json` =1.0.154; `spikes/browse-core/Cargo.lock` committed |
| PowerShell | 7.6.9 |
| ComfyUI | 0.42.1 (S0.4 only; install folder and Python passed as parameters, never recorded) |
| Spike code | `spikes/browse-core/`, in the commit that adds it (recorded in §5) |

## 3. Method

`spikes/browse-core/` is a standalone crate (not a workspace member; the root `Cargo.toml` excludes `spikes`) with four console binaries. None opens a window. `run-spike.ps1` builds them in release, runs each check and writes `spikes/browse-core/results/checks.json` (one verdict per check, raw samples in `results/raw/<check>/`, git-ignored). A check that runs again replaces only its own entry in `checks.json`; its earlier raw folder is moved aside first. Generated inputs live in `spikes/browse-core/work/` (git-ignored) and are removed at the end.

```powershell
pwsh -NoProfile -File spikes/browse-core/run-spike.ps1 -Only pdh-read,decode,facets      # Step 2
pwsh -NoProfile -File spikes/browse-core/run-spike.ps1 -Only pdh-load,spill `
    -ComfyRoot <ComfyUI folder> -Python <python.exe> -Checkpoint <SDXL checkpoint name>   # Step 3, owner says go
pwsh -NoProfile -File spikes/browse-core/run-spike.ps1 -Only spill -ComfyRoot ... -DryRun # prints what S0.4 would start
```

### S0.1 PDH (`pdh-probe`)

- **`read` (Step 2):** records whether the process token is elevated (`GetTokenInformation(TokenElevation)`), then finds the discrete GPU's LUID through DXGI (the hardware adapter with the most dedicated video memory). One PDH query holds the three counters, added with `PdhAddEnglishCounterW` and the wildcard instance: `\GPU Adapter Memory(*)\Dedicated Usage`, `\GPU Adapter Memory(*)\Shared Usage`, `\GPU Engine(*)\Utilization Percentage`. After a first collect, 300 collects run 100 ms apart in the one process; each `PdhCollectQueryData` call is timed alone, and the formatted arrays (`PdhGetFormattedCounterArrayW`) are read after each collect and timed separately. "Values arrive" means: at least one instance of each counter whose name holds the discrete GPU's `luid_0x…_0x…` and whose status is valid. The instance names are recorded with the LUIDs, PIDs and numbers replaced by placeholders.
- **`load` (Step 3):** a D3D11 device on the discrete adapter. The adapter's `Shared Usage` is sampled at 4 Hz for 10 s idle, then while 1 GiB of D3D11 `STAGING` buffers (16 × 64 MiB, each mapped and written once) is held for 10 s, then for 10 s after they are released. Then a 30 s compute loop (a long ALU loop in a compute shader, compiled at run time with `D3DCompile`, dispatched continuously with a query wait every few dispatches so the queue doesn't run away) runs while, at 1 Hz, the busiest engine type is read (per engine, `pid_*…eng_<n>_engtype_<type>` instances summed over processes; per type, its busiest engine; then the busiest type) and NVML's utilisation (`nvml.dll`, loaded in process, `nvmlDeviceGetUtilizationRates` on device 0) is read. The medians of the two 1 Hz series are compared.

### S0.2 Decode cost (`decode-bench`)

- **Inputs**, generated in `work/`: a 1024² PNG, a 2048² JPEG (quality 90), a 1024² WebP (lossless, the `image` crate's only WebP encoder) and a 4096² PNG. The pixels are smooth gradients with deterministic noise, so the files don't compress to nothing.
- **`timing`:** on one thread, each input is read from disk, decoded with `image::ImageReader` under P4b-R36's limits (256 MiB allocation, 16,384 px a side), resized with `DynamicImage::resize` (Lanczos3, long side to the target) and PNG-encoded in memory, at 160 and 512 px, and the 1024² PNG also at 256 px for the bar: 100 runs of the 1024² PNG → 256 px, 30 runs of each other pair, after 3 untimed warm-up runs.
- **`bomb`, in its own process:** a PNG with a valid IHDR claiming 60,000 × 60,000 (RGBA 8-bit), with a valid but short IDAT, is decoded under the same limits. The peak working set (`GetProcessMemoryInfo`) is compared with the working set measured just before the decode.
- **`broken`:** a JPEG cut to half its length (inside the scan), a JPEG cut to 200 bytes (inside the headers), and an empty file, each decoded under `catch_unwind`.
- **`concurrent`, in its own process:** four threads, released together by a barrier, each read, decode (256 MiB limit), resize to 512 px and encode the 4096² PNG. Peak working set against the working set before the threads start.
- **`cold`:** 10,000 512² JPEGs are generated (untimed), then `min(4, cores/2)` worker threads render each at 160 px and write the PNG to a fresh cache folder: the total time. The OS file cache is not flushed (that needs admin), so "cold" means the thumbnail cache was empty, not the disk cache.

### S0.3 Facets (`facet-bench`)

A synthetic `index.db` in `work/` (WAL, `synchronous=NORMAL`, as the index today) with today's `assets` table plus a draft of Task 5's additions, built from the plan's Global Constraints (`assets.query`, Takes, Manifest fields) and today's `crates/lil-core/src/index/schema.rs`. The draft is a guess, Task 5 owns the real one:

- `assets` gains `rating INTEGER NOT NULL DEFAULT 0` and `label TEXT` (NULL for none), with indexes on each;
- `asset_tags (asset INTEGER, tag_key TEXT, tag TEXT, PRIMARY KEY (asset, tag_key)) WITHOUT ROWID` with an index on `(tag_key, asset)`; `tag_key` is the lowercased tag, for "any of, case-insensitive";
- `runs (asset INTEGER, run_id TEXT, state TEXT, PRIMARY KEY (asset, run_id)) WITHOUT ROWID` (`state` `open`, `signed_off` or `trashed`);
- `takes (asset INTEGER, run_id TEXT, take INTEGER, flag TEXT, rating INTEGER, PRIMARY KEY (asset, run_id, take)) WITHOUT ROWID` with a partial index on `(flag, asset) WHERE flag IS NOT NULL`.

Data: 10 projects; 10,000 and 50,000 assets; the ten statuses and 8 types spread unevenly; 3 distinct tags each from 200; ratings 0–5 and labels (five, or none) spread; 1–3 runs each with 4 takes, about 10 % of takes flagged `pick` and 5 % `reject`, about 10 % of runs trashed (their flags don't count, P4b-R40).

The measured unit is one read transaction holding: the page (`SELECT … ORDER BY asset_id, project LIMIT 201` with each row's tags and open-run count by correlated subquery), `count(*)` with the same filters, and the six facet counts, each disjunctive (P4b-R10: every filter except its own): `status`, `type`, `rating`, `label` by `GROUP BY`; `tags` by a join on `asset_tags` grouped by tag, the 100 largest (ties by value) plus whether more exist; `flag` as three counts (`pick`, `reject`, none). Two filter sets: **none** (the opening view) and **five filters** (`status` any of 2, `tags` any of 2, `label` any of `red` and none, `rating_min` 2, `flag` `pick`), so each facet has its own WHERE. 5 warm-up runs, then 100 timed runs of each set at each size, on one connection opened read-only. `EXPLAIN QUERY PLAN` of every statement is recorded. The verdict takes the slower filter set at 10,000.

### S0.4 Spill margin (`spill-probe`, Step 3 only)

- **Safety:** the script takes the ComfyUI folder (`-ComfyRoot`) and its Python (`-Python`) as parameters, and refuses to start if anything listens on the chosen port (default 8199, not ComfyUI's 8188) or if any process runs from that folder, names it on its command line, or is that Python running a `main.py`. It starts `main.py --listen 127.0.0.1 --port <port> --disable-auto-launch --disable-all-custom-nodes` with `--user-directory`, `--input-directory`, `--output-directory` and `--temp-directory` all in the run's own folder (`results/raw/spill/comfy/`), and `PYTHONDONTWRITEBYTECODE=1`, so the install's settings, custom nodes and models are only read. Its console output goes to a file in the run folder; no window opens. A `try`/`finally` stops the whole process tree it started (`Process.Kill($true)`), also after an error or Ctrl+C. `-DryRun` prints the refusal checks' result and the exact command line, and starts nothing.
- **Graphs:** the spike's own, written as API-format JSON into the run folder: `CheckpointLoaderSimple` (an SDXL checkpoint named by `-Checkpoint`), two `CLIPTextEncode`, `EmptyLatentImage`, `KSampler`, `VAEDecode`, `SaveImage`. The fitting graph is 1024 × 1024, batch 1, 20 steps; the spilling graph defaults to 2048 × 2048, batch 4, 10 steps (parameters `-SpillWidth`, `-SpillHeight`, `-SpillBatch`, `-SpillSteps`). That this graph spills on a 16 GB card is a guess; the run records whether it did.
- **Sampling:** `spill-probe` reads `\GPU Process Memory(*)\Shared Usage` and `Dedicated Usage` at 1 Hz and sums the instances of the discrete GPU's LUID whose PID is ComfyUI's process or one of its descendants (re-listed each second), as P4b-R37 defines the engine; it also records the adapter's `Shared Usage`. Phases: 60 s idle after ComfyUI answers `/system_stats`, then 3 fitting runs, then 3 spilling runs, one at a time (each until `/history` has it, or a 900 s timeout and `/interrupt`). Idle level: the minimum over the idle 60 s (P4b-R37's minimum). Per fitting run: the maximum above idle. Per spilling run: the 10 s sustained level above idle, the largest minimum over any 10 consecutive 1 Hz samples. M: the largest multiple of 128 MiB at or below the middle of the gap; PASS needs it strictly inside the gap.

## 4. Pass criteria

Copied from the plan (Task 0) before anything ran.

- **S0.1 PDH without admin** (CORE-059): from a process whose token isn't elevated, `\GPU Adapter Memory(*)\Dedicated Usage`, `\GPU Adapter Memory(*)\Shared Usage` and `\GPU Engine(*)\Utilization Percentage` (English names, `PdhAddEnglishCounterW`) add and collect for the discrete GPU's LUID. PASS = values arrive for all three, and a collect's p99 < 5 ms over 300 calls. Recorded, not bars (fix round 1, M11): the instance-name grammar; how the adapter's `Shared Usage` moves while 1 GiB of D3D11 `STAGING` buffers is held and released; during a 30 s D3D11 compute loop, the busiest engine type's sum over processes against NVML's utilisation (median of 1 Hz samples). The load step (the buffers and the loop, about a minute) runs only after the owner says go. FAIL → CORE-059's "not shown" path: Task 19 keeps NVML's headline and sends `null`.
- **S0.2 Decode cost** (APP-038; P4b-R12): on generated inputs (1024² PNG, 2048² JPEG, 1024² WebP, 4096² PNG), decode + resize (Lanczos3) + PNG encode to 160 and 512 px on one thread. PASS = p95 of a 1024² PNG → 256 px < 60 ms; a PNG header claiming 60,000 × 60,000 is refused by the `image` limits with peak working set under 64 MiB above baseline; a truncated JPEG and an empty file return errors without panic; four concurrent 4096² decodes under the 256 MiB per-decode limit (P4b-R36) keep the working set under 1.5 GiB above baseline. Recorded: a cold cache of 10,000 512² JPEGs at 160 px with `min(4, cores/2)` workers (total time). FAIL → P4b-R12's helper-process route.
- **S0.3 Facets** (APP-138, CORE-042; P4b-R10): a synthetic `index.db` with Task 5's draft schema, 10,000 and 50,000 assets, 3 tags each from 200, labels and ratings spread, 1–3 runs with 4 takes each. PASS = `assets.query`'s page of 200 plus the six disjunctive facet counts in one read transaction, p95 over 100 runs at 10,000 < 100 ms. Recorded: the 50,000 p95 and the plan of each query (`EXPLAIN QUERY PLAN`). FAIL → the owner hears before Task 1 (P4b-R23).
- **S0.4 Spill margin** (APP-132; P4b-R19; owner says go): an owned ComfyUI started by the script on `127.0.0.1`; record the engine process's PDH `Shared Usage` at idle for 60 s, during 3 runs of a workflow that fits the card, and during 3 runs sized to exceed it (the spike's own graphs). PASS = a margin M exists with every fitting run's max above idle < M < every spilling run's 10 s sustained level above idle; M rounded down to 128 MiB is the value. FAIL (no clean gap) → 512 MiB stays and the overlap is recorded.

## 5. Measurements

Not run yet.

## 6. Verdict

Not run yet.

## 7. Decision

Not run yet.

## 8. Follow-ups

Not run yet.
