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
| CPU and memory | a desktop CPU with 32 logical processors (so `min(4, cores/2)` = 4), and enough RAM that the 1.5 GiB bar isn't near it |
| Rust | rustc / cargo 1.99.0, `stable-x86_64-pc-windows-msvc`, edition 2024 |
| Crates | `windows` =0.62.2, `image` =0.25.10 (`png`, `jpeg`, `webp`), `rusqlite` =0.40.2 (`bundled`, the workspace's version), `serde_json` =1.0.154; `spikes/browse-core/Cargo.lock` committed |
| PowerShell | 7.6.9 |
| ComfyUI | 0.42.1 (S0.4 only; install folder and Python passed as parameters, never recorded) |
| Spike code | `spikes/browse-core/`: S0.1's readability part and S0.2 ran at commit `27d580a3`; S0.3 at `7a8a2464` (fix round 1); S0.1's load part and S0.4 at `d1bbe6b9` (Step 3), with S0.4's samples analysed again under PF-26 in the Step 4 commit |

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
- **`load` (Step 3):** a D3D11 device on the discrete adapter. The adapter's `Shared Usage` is sampled at 4 Hz for 10 s idle, then while 1 GiB of D3D11 `STAGING` buffers (16 × 64 MiB, each mapped and written once) is held for 10 s, then for 10 s after they are released. Then a 30 s compute loop (a long ALU loop in a compute shader, compiled at run time with `D3DCompile`, dispatched continuously with a query wait every few dispatches so the queue doesn't run away) runs while, at 1 Hz, the busiest engine type is read (per engine, `pid_*…eng_<n>_engtype_<type>` instances summed over processes; per type, its busiest engine; then the busiest type) and NVML's utilisation (`nvml.dll`, loaded in process from `System32` only, `nvmlDeviceGetUtilizationRates` on device 0, whose PCI device id is checked against the DXGI adapter's) is read. The medians of the two 1 Hz series are compared.

### S0.2 Decode cost (`decode-bench`)

- **Inputs**, generated in `work/`: a 1024² PNG, a 2048² JPEG (quality 90), a 1024² WebP (lossless, the `image` crate's only WebP encoder) and a 4096² PNG. The pixels are smooth gradients with deterministic noise, so the files don't compress to nothing.
- **`timing`:** on one thread, each input is read from disk, decoded with `image::ImageReader` under P4b-R36's limits (256 MiB allocation, 16,384 px a side), resized with `DynamicImage::resize` (Lanczos3, long side to the target) and PNG-encoded in memory, at 160 and 512 px, and the 1024² PNG also at 256 px for the bar: 100 runs of the 1024² PNG → 256 px, 30 runs of each other pair, after 3 untimed warm-up runs.
- **`bomb`, in its own process:** a PNG with a valid IHDR claiming 60,000 × 60,000 (RGBA 8-bit), with a valid but short IDAT, is decoded under the same limits. The peak working set (`GetProcessMemoryInfo`) is compared with the working set measured just before the decode.
- **`broken`:** a JPEG cut to half its length (inside the scan), a JPEG cut to 200 bytes (inside the headers), and an empty file, each decoded under `catch_unwind`.
- **`concurrent`, in its own process:** four threads, released together by a barrier, each read, decode (256 MiB limit), resize to 512 px and encode the 4096² PNG. Peak working set against the working set before the threads start.
- **`cold`:** 10,000 512² JPEGs are generated (untimed), then `min(4, cores/2)` worker threads render each at 160 px and write the PNG to a fresh cache folder: the total time. The OS file cache is not flushed (that needs admin), so "cold" means the thumbnail cache was empty, not the disk cache.

### S0.3 Facets (`facet-bench`)

A synthetic `index.db` in `work/` (WAL, `synchronous=NORMAL`, as the index today) with Task 5's draft schema: today's `assets` table (`crates/lil-core/src/index/schema.rs`) with the "schema 2 adds" of the plan's Task 5 Interfaces block (fix round 1, I1; the first runs used an invented schema, and their figures are replaced):

- `assets` gains `rating INTEGER NOT NULL DEFAULT 0`, `label TEXT`, `runs_waiting INTEGER NOT NULL DEFAULT 0`, `external INTEGER NOT NULL DEFAULT 0`, `working_doc INTEGER NOT NULL DEFAULT 0`, `display TEXT`, `last_change TEXT` and `repair TEXT`; today's three indexes stay, and none is added on `rating` or `label` (the plan names none);
- `asset_tags (project, asset_id, tag, tag_fold)` with an index on `tag_fold`;
- `take_marks (project, asset_id, run_id, take, flag, rating)` with an index on `flag`.

What the plan leaves open, and this spike chose:
- the primary keys: `asset_tags` `PRIMARY KEY (project, asset_id, tag_fold) WITHOUT ROWID` (a tag once per asset, case-insensitively), `take_marks` `PRIMARY KEY (project, asset_id, run_id, take) WITHOUT ROWID`; the page's per-row tag lookup uses the first;
- `tag_fold` is the lowercased tag; the facet shows `min(tag)` for each `tag_fold`;
- P4b-R40: the mirror leaves a trashed run's takes out of `take_marks`, so the flag queries need no run state;
- the JSON columns' contents: `display` about 150 bytes (`{path, sha256, media_kind}`, on 90 % of assets), `last_change` about 60 bytes (`{to, by, utc}`, 95 %), `repair` on 1 %; `external` on 5 %, `working_doc` on 10 %;
- `files.kind` and the `files_sha` index are left out: no facet or filter measured here reads `files` (`duplicates` isn't measured).

Data: 10 projects; 10,000 and 50,000 assets; the ten statuses and 8 types spread unevenly; 3 distinct tags each from 200; ratings 0–5 and labels (five, or none) spread; 1–3 runs each with 4 takes, about 10 % of takes flagged `pick` and 5 % `reject`, about 10 % of runs trashed (not mirrored); `runs_waiting` counts the open runs (about 30 % of runs).

The measured unit is one read transaction holding: the page (`SELECT … ORDER BY asset_id, project LIMIT 201` with every `assets` column the summary needs, including `runs_waiting` and the JSON columns, and each row's tags by correlated subquery), `count(*)` with the same filters, and the six facet counts, each disjunctive (P4b-R10: every filter except its own): `status`, `type`, `rating`, `label` by `GROUP BY`; `tags` grouped by `tag_fold`, the 100 largest (ties by value) plus whether more exist, joined to `assets` only when another filter applies; `flag` as three counts (`pick`, `reject`, none). The `tags` and `flag` filters are `(project, asset_id) IN (SELECT project, asset_id FROM … WHERE tag_fold IN (…) / flag = …)`, which use the two named indexes. Two filter sets: **none** (the opening view) and **five filters** (`status` any of 2, `tags` any of 2, `label` any of `red` and none, `rating_min` 2, `flag` `pick`), so each facet has its own WHERE. 5 warm-up runs, then 100 timed runs of each set at each size, on one connection opened read-only. `EXPLAIN QUERY PLAN` of every statement is recorded. The verdict takes the slower filter set at 10,000.

### S0.4 Spill margin (`spill-probe`, Step 3 only)

- **Safety** (with fix round 1's changes):
  - The script takes the ComfyUI folder (`-ComfyRoot`) and its Python (`-Python`) as parameters.
  - It refuses to start if anything listens on the chosen port (default 8199) or on ComfyUI's default `127.0.0.1:8188` (another ComfyUI would share the GPU), if any process runs from that folder, names it on its command line, or is a `python*.exe` running a `main.py`, or if the install's `comfy/cli_args.py` lacks any flag the script passes.
  - It starts `main.py --listen 127.0.0.1 --port <port> --disable-auto-launch --disable-all-custom-nodes --disable-assets` with `--user-directory`, `--input-directory`, `--output-directory`, `--temp-directory` and `--previews-directory` all in the run's own folder (`results/raw/spill/comfy/`), and `PYTHONDONTWRITEBYTECODE=1`, so the install's settings, custom nodes and models are only read. Its console output goes to a file in the run folder.
  - ComfyUI and the probe run in a kill-on-close job object that only the script's pwsh holds. Each is created suspended, assigned to the job and then resumed (a failed resume terminates it; one that exits within a second is an error naming its exit code), so every process it starts is in the job too. When pwsh ends for any reason (an error, Ctrl+C, a closed console, a killed process), Windows ends them. The orderly path still stops them in a `try`/`finally` (`Process.Kill($true)`), and the probe also stops when ComfyUI's process exits.
  - Each child gets `CREATE_NO_WINDOW` (a console without a window, which its own console children share), so no window opens whatever the host, and inherits only its three standard handles (NUL and the two log files, through `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`).
  - The probe logs why it stops, or why it can't watch ComfyUI; if it writes no sample within 15 s of its start, the run fails at once.
  - `-SelfTestJob` checks the job with no ComfyUI: with a `ping -t 127.0.0.1` stand-in (fix round 1), and with the install's venv `python.exe` running a one-line script that starts a grandchild `python -c` (fix round 2; standard library only). Killing the script's pwsh ended every process of the stand-in's tree, and the venv launcher's child and grandchild reported no console window.
  - Every REST call has a timeout (10 s; 30 s for `/prompt` and `/object_info`); 30 failed polls in a row end a run as `unreachable`. After a timed-out run the script interrupts it and waits (at most 60 s) for ComfyUI's queue to empty. Any run that doesn't end in `success` makes the verdict INVALID.
  - After the stop, the script checks again that nothing is left: the port and the process checks. Since Step 3 (§5), it retries for up to 10 s before reporting, because a port can still answer for a moment while its socket closes.
  - `-DryRun` prints the refusal checks' result and the exact command line, and starts nothing.
- **Graphs:** the spike's own, written as API-format JSON into the run folder: `CheckpointLoaderSimple` (an SDXL checkpoint named by `-Checkpoint`), two `CLIPTextEncode`, `EmptyLatentImage`, `KSampler`, `VAEDecode`, `SaveImage`. The fitting graph is 1024 × 1024, batch 1, 20 steps; the spilling graph defaults to 2048 × 2048, batch 4, 10 steps (parameters `-SpillWidth`, `-SpillHeight`, `-SpillBatch`, `-SpillSteps`). Whether this graph spills on a 16 GB card is unknown until it runs; the run records whether it did.
- **Sampling:** `spill-probe` reads `\GPU Process Memory(*)\Shared Usage` and `Dedicated Usage` at 1 Hz and sums the instances of the discrete GPU's LUID whose PID is ComfyUI's process or one of its descendants (re-listed each second), as P4b-R37 defines the engine; it also records the adapter's `Shared Usage`. Phases: 60 s idle after ComfyUI answers `/system_stats` (idle 0), then 3 fitting runs, then a second 60 s idle with the model resident (idle 1, recorded), then 3 spilling runs, one at a time (each until `/history` has it, or a 900 s timeout and `/interrupt`). Idle level: the minimum over idle 0 (P4b-R37's minimum). Per fitting run: the maximum above idle. Per spilling run: the 10 s sustained level above idle, the largest minimum over any 10 consecutive 1 Hz samples. PASS needs a multiple of 128 MiB strictly inside the gap, else FAIL ("gap narrower than the 128 MiB step"; fix round 1, I2). The value: as Step 3 ran, the middle of the gap rounded down to 128 MiB; after it, PF-26 (§7) chose the smallest multiple of 128 MiB inside the gap that is at least twice the fitting runs' maximum, and the recorded samples were analysed again with that rule (`spill-probe analyze`, no new run); the midpoint figure is kept beside it. Recorded too: both margins against idle 1, which is closer to P4b-R37's idle in production.

## 4. Pass criteria

Copied from the plan (Task 0) before anything ran.

- **S0.1 PDH without admin** (CORE-059): from a process whose token isn't elevated, `\GPU Adapter Memory(*)\Dedicated Usage`, `\GPU Adapter Memory(*)\Shared Usage` and `\GPU Engine(*)\Utilization Percentage` (English names, `PdhAddEnglishCounterW`) add and collect for the discrete GPU's LUID. PASS = values arrive for all three, and a collect's p99 < 5 ms over 300 calls. Recorded, not bars (fix round 1, M11): the instance-name grammar; how the adapter's `Shared Usage` moves while 1 GiB of D3D11 `STAGING` buffers is held and released; during a 30 s D3D11 compute loop, the busiest engine type's sum over processes against NVML's utilisation (median of 1 Hz samples). The load step (the buffers and the loop, about a minute) runs only after the owner says go. FAIL → CORE-059's "not shown" path: Task 19 keeps NVML's headline and sends `null`.
- **S0.2 Decode cost** (APP-038; P4b-R12): on generated inputs (1024² PNG, 2048² JPEG, 1024² WebP, 4096² PNG), decode + resize (Lanczos3) + PNG encode to 160 and 512 px on one thread. PASS = p95 of a 1024² PNG → 256 px < 60 ms; a PNG header claiming 60,000 × 60,000 is refused by the `image` limits with peak working set under 64 MiB above baseline; a truncated JPEG and an empty file return errors without panic; four concurrent 4096² decodes under the 256 MiB per-decode limit (P4b-R36) keep the working set under 1.5 GiB above baseline. Recorded: a cold cache of 10,000 512² JPEGs at 160 px with `min(4, cores/2)` workers (total time). FAIL → P4b-R12's helper-process route.
- **S0.3 Facets** (APP-138, CORE-042; P4b-R10): a synthetic `index.db` with Task 5's draft schema, 10,000 and 50,000 assets, 3 tags each from 200, labels and ratings spread, 1–3 runs with 4 takes each. PASS = `assets.query`'s page of 200 plus the six disjunctive facet counts in one read transaction, p95 over 100 runs at 10,000 < 100 ms. Recorded: the 50,000 p95 and the plan of each query (`EXPLAIN QUERY PLAN`). FAIL → the owner hears before Task 1 (P4b-R23).
- **S0.4 Spill margin** (APP-132; P4b-R19; owner says go): an owned ComfyUI started by the script on `127.0.0.1`; record the engine process's PDH `Shared Usage` at idle for 60 s, during 3 runs of a workflow that fits the card, and during 3 runs sized to exceed it (the spike's own graphs). PASS = a margin M exists with every fitting run's max above idle < M < every spilling run's 10 s sustained level above idle; M rounded down to 128 MiB is the value. FAIL (no clean gap) → 512 MiB stays and the overlap is recorded.

## 5. Measurements

Step 2 ran on 2026-11-05 from a normal, non-elevated shell: `pwsh -NoProfile -File spikes/browse-core/run-spike.ps1 -Only pdh-read,decode,facets` (254 s with the build already done). `facets` ran twice more: once with the 64 MiB-cache rows added, and once in fix round 1 on Task 5's draft schema, whose figures replace both earlier runs (§5's S0.3). The code of each is the commit in §2. Nothing else was started on the GPU for the run; the desktop was in normal use. Raw data: written to `spikes/browse-core/results/` (raw output kept privately).

Step 3 ran on 2026-11-05 after the owner said go, from a normal, non-elevated shell: `pwsh -NoProfile -File spikes/browse-core/run-spike.ps1 -Only pdh-load,spill` with the install's folder, its Python and an SDXL checkpoint as parameters. It took about 7 minutes and exited 0.

### S0.1 PDH, the readability part (Step 2)

The token was not elevated (`TokenElevation` 0). All three counters added with `PdhAddEnglishCounterW` (status 0), and the first collect returned 0.

| Metric | n | p50 | p95 | p99 | max | Unit |
|---|---|---|---|---|---|---|
| `PdhCollectQueryData`, the three wildcard counters, 100 ms apart | 300 | 1.016 | 1.242 | **1.463** | 1.71 | ms |
| `PdhGetFormattedCounterArrayW`, the three counters after each collect | 300 | 1.152 | 1.523 | 1.716 | 2.027 | ms |

- **Values arrive:** every one of the 300 collects returned a valid value for the discrete GPU's LUID from all three counters, with no collect error.
- **Instance names** (LUIDs, PIDs and numbers replaced):
  - `GPU Adapter Memory`: `luid_<luid>_phys_<n>`, one instance per adapter. PDH listed 3 adapters; DXGI counted 2 hardware adapters (the third is presumably Windows' own software adapter; not checked).
  - `GPU Engine`: `pid_<pid>_luid_<luid>_phys_<n>_eng_<n>_engtype_<type>`, one instance per process and engine: about 1,285 instances in all, 768 of them on the discrete GPU (48 processes × 16 engines).
  - The discrete GPU's engine types: `3D`, `Copy` (6 engines), `VideoDecode` (2), `VideoEncode` (2), `JPEG_Decode_1`, `OFA_0`, `Security`, `Security_1`, `VR`. It showed **no `Compute` engine type**, so compute work on it is expected under `3D`; Step 3's compute loop will show which.
  - The other GPU's types contain spaces (`High Priority 3D`, `Compute 0`, `Video Codec 0`), so a parser must take everything after `engtype_` as the type.

Verdict against the bar: **PASS** (values for all three; collect p99 1.463 ms < 5 ms).

### S0.2 Decode cost

One thread. Each run reads the file, decodes under P4b-R36's limits (256 MiB allocation, 16,384 px a side), resizes (Lanczos3) and PNG-encodes in memory. Inputs: 1024² PNG 2.56 MiB, 2048² JPEG 1.50 MiB, 1024² WebP (lossless) 1.16 MiB, 4096² PNG 40.97 MiB.

| Input → px | n | p50 | p95 | p99 | max | Unit |
|---|---|---|---|---|---|---|
| 1024² PNG → 160 | 30 | 12.0 | 12.6 | 12.7 | 12.7 | ms |
| **1024² PNG → 256** | 100 | 12.6 | **13.5** | 13.9 | 15.1 | ms |
| 1024² PNG → 512 | 30 | 16.1 | 17.9 | 18.4 | 18.4 | ms |
| 2048² JPEG → 160 | 30 | 51.5 | 67.4 | 67.4 | 67.4 | ms |
| 2048² JPEG → 512 | 30 | 76.0 | 78.3 | 80.8 | 80.8 | ms |
| 1024² WebP → 160 | 30 | 25.6 | 26.4 | 29.0 | 29.0 | ms |
| 1024² WebP → 512 | 30 | 31.6 | 32.0 | 33.5 | 33.5 | ms |
| 4096² PNG → 160 | 30 | 325.4 | 330.0 | 330.6 | 330.6 | ms |
| 4096² PNG → 512 | 30 | 335.2 | 342.8 | 343.0 | 343.0 | ms |

- **The stages** (p50): decode is 5 ms for the 1024² PNG, 14 ms for the WebP, 20–24 ms for the 2048² JPEG and 98 ms for the 4096² PNG; the Lanczos3 resize is the larger part from 2048² up (30–50 ms for the JPEG, 213–219 ms for the 4096² PNG); the PNG encode is 0.1–1.7 ms.
- **60,000 × 60,000 header:** refused (`Image size exceeds limit`) in 0.05 ms; peak working set **+0.1 MiB** above the baseline (8.4 MiB). Recorded: with only the 256 MiB allocation limit, and with the crate's default limits, it is refused too (`Memory limit exceeded`), and the peak stays at +0.2 MiB.
- **Broken files**, each under `catch_unwind`; **no panic** in any:
  - the empty file: an error (`The image format could not be determined`);
  - the JPEG cut to 200 bytes (inside its headers): an error (`Not enough bytes`);
  - **the JPEG cut to half its length (inside the scan): no error.** The decoder returned a whole 2048 × 2048 image (`Ok`), the missing half filled in.
- **Four concurrent 4096² renders** (decode under the 256 MiB limit, resize to 512, encode; released together by a barrier): all four decoded, 79–81 ms each, 270 ms wall; peak working set **+487.5 MiB** above the baseline.
- **Recorded, the cold cache:** 10,000 generated 512² JPEGs (818 MiB) at 160 px with `min(4, 32/2)` = 4 workers: **13.6 s** in all (735 a second; per item p50 5.5 ms, p95 6.3 ms, max 71 ms), no failures. The inputs had just been written, so they were in the OS file cache.

Verdict against the bar: **FAIL**, on one item: the JPEG truncated inside its scan decodes without an error. Every other item passes: the 1024² PNG → 256 px p95 is 13.5 ms (< 60 ms); the header is refused at +0.1 MiB (< 64 MiB); the empty file and the JPEG truncated in its headers return errors; nothing panics; four concurrent 4096² decodes stay at +487.5 MiB (< 1.5 GiB).

### S0.3 Facets

Measured again in fix round 1 on Task 5's draft schema (§3), with `-Only facets` (207 s); these figures replace the first runs' on the invented schema. `index.db` as in §3 (SQLite 3.53.2, `ANALYZE` run after the build). 10,000 assets: 30,000 tag rows, 72,312 mirrored takes, 6,033 open runs, 14.0 MiB. 50,000 assets: 150,000 tag rows, 360,196 mirrored takes, 30,019 open runs, 71.1 MiB. One read transaction holds the page, the total and the six facets; 100 timed runs after 5 warm-up runs, on one read-only connection.

| Assets, filters, page cache | n | p50 | p95 | p99 | max | Unit |
|---|---|---|---|---|---|---|
| **10,000, none, default** | 100 | 50.6 | **69.8** | 72.4 | 73.3 | ms |
| 10,000, five filters, default | 100 | 32.4 | 50.7 | 52.3 | 52.6 | ms |
| 50,000, none, default | 100 | 820.8 | **980.1** | 1,036.3 | 1,059.2 | ms |
| 50,000, five filters, default | 100 | 246.0 | 340.9 | 360.8 | 363.3 | ms |
| **10,000, none, 64 MiB** | 100 | 30.8 | **52.4** | 52.6 | 53.4 | ms |
| 10,000, five filters, 64 MiB | 100 | 24.0 | 34.4 | 34.8 | 36.2 | ms |
| **50,000, none, 64 MiB** | 100 | 196.6 | **301.3** | 326.8 | 340.5 | ms |
| 50,000, five filters, 64 MiB | 100 | 160.8 | 200.1 | 202.2 | 204.7 | ms |

"Default" is SQLite's default page cache (2 MiB), which today's index uses (`schema.rs` sets no `cache_size`); the bar's verdict uses it, as §3 fixed before the first run. The 64 MiB rows set `PRAGMA cache_size` on the reading connection: the cache the owner chose (§7).

- **Spread between runs:** a 10,000-asset check of the same code just before the measured run gave p95 40.1 ms (none) and 29.5 ms (five) at the default cache, against 69.8 and 50.7 here, so the run-to-run spread on this desktop is large (it was in normal use).
- **Where the time goes** (each statement alone, p50 of 20):
  - At 10,000, default cache, no filter: `flag` 19.3 ms, `tags` 17.6 ms, the page 5.7 ms, the other facets 3–5 ms. With five filters, `tags` 13.1 ms and everything else under 5 ms (`flag` 0.3 ms).
  - At 50,000, default cache, no filter: `tags` 471 ms and `flag` 144 ms take most of it; the other facets and the page are 16–28 ms each. With five filters: `tags` 100 ms, the rest 10–26 ms.
  - With 64 MiB at 50,000, no filter: `flag` 81 ms, `tags` 59 ms, the rest under 9 ms.
- **Query plans** (`EXPLAIN QUERY PLAN`, the same at both sizes):
  - No filter: the page scans `assets` and sorts with a temporary B-tree (no index on `asset_id` alone, as today), each row's tags by `asset_tags`' primary key. `status`, `type`, `rating` and `label` scan `assets` and group with a temporary B-tree. `tags` scans the `tag_fold` index, which doesn't hold `tag`, so `min(tag)` looks every tag row up by the primary key: that is most of the 471 ms. `flag` runs two correlated `EXISTS` per asset through `take_marks_flag` (`flag`, then the key columns in the index).
  - Five filters: SQLite drives from the `tags` and `flag` lists (`tag_fold` and `flag` indexes) into `assets` by its `(project, asset_id)` unique index, for the page, the total and every facet.
  - The first form of the flag facet, a row-value `(project, asset_id) IN (SELECT …)` in the result columns, took about 2 s at 10,000 assets (SQLite searched its list per row); the measured form uses `EXISTS` on the key, and the filter keeps the `IN` form in `WHERE`, where SQLite plans it well.
  - Every plan is in the raw output.
- **The budgets (P4b-R23: twice the p95, rounded up to 10 ms):**
  - default cache: at 10,000, 2 × 69.8 → **140 ms**; at 50,000, 2 × 980.1 → **1,970 ms**;
  - 64 MiB: at 10,000, 2 × 52.4 → **110 ms**; at 50,000, 2 × 301.3 → **610 ms**.
  - Both 10,000 budgets are over 100 ms.

Verdict against the bar: **PASS** (p95 at 10,000 with the default cache: 69.8 ms with no filter, 50.7 ms with five; both < 100 ms).

### S0.1's load part (Step 3)

The token was not elevated. One D3D11 device on the discrete adapter.

| Phase, the adapter's `Shared Usage` at 4 Hz | n | median | min | max | Unit |
|---|---|---|---|---|---|
| idle, 10 s | 40 | 116.9 | 111.7 | 125.5 | MiB |
| 16 × 64 MiB `STAGING` buffers held (each mapped and written; 115 ms to create and write), 10 s | 40 | 112.0 | 112.0 | 190.5 | MiB |
| after release, 10 s | 40 | 112.8 | 112.0 | 117.9 | MiB |

- **`STAGING` buffers:** the adapter's `Shared Usage` **did not respond** to 1 GiB of them (held − idle: −5.0 MiB, one 190.5 MiB sample). The adapter's `Dedicated Usage` didn't move either (median 3,068 → 3,038 → 3,049 MiB). So 1 GiB of CPU-side D3D11 staging memory isn't counted in either figure on this driver.
- **The 30 s compute loop** (4,086 batches of 8 dispatches): the busiest engine type was `3D` in every one of the 29 samples that had one (no `Compute` type on this GPU, as the readability part found). The busiest engine's sum over processes: median **65.5 %** (p95 66.8, max 67.1, n 29); NVML: median **65.0 %** (p95 69, max 69, n 30). NVML was loaded from `System32` and its device 0 is the DXGI adapter (PCI id matched; one NVML device). The loop kept the GPU about two-thirds busy, so the comparison is at a partial load, not at saturation.

Recorded, no bar: the load part needs no verdict (fix round 1, M11).

### S0.4 Spill margin (Step 3)

ComfyUI 0.42.1 started on `127.0.0.1:8199` with its own folders, custom nodes and the assets system off, and stopped at the end; its dynamic VRAM was on (ComfyUI's default). The engine is ComfyUI's process tree: 3 processes (the venv launcher, its console host and the interpreter), and only the interpreter had a `GPU Process Memory` instance. 327 samples at 1 Hz, no collect error. Every run ended in `success`; nothing timed out.

| Phase | Time | Engine `Shared Usage` | Engine `Dedicated Usage` | Unit |
|---|---|---|---|---|
| idle 0, 60 s after start | | 74 (flat) | 228 | MiB |
| fit 1 (first model load) / 2 / 3 | 26 / 4 / 4 s | 74 → 276, then 276 | up to 7,262 / 7,328 / 7,328 | MiB |
| idle 1, 60 s, model resident | | 276 (flat) | 6,976 | MiB |
| spill 1 / 2 / 3 | 37 / 36 / 37 s | 276 → 5,073, then 5,073 throughout runs 2 and 3 | 363 – 12,832 / 363 – 11,883 / 363 – 11,755 | MiB |

Against idle 0 (74 MiB), the verdict's idle:

| Measure | Run 1 | Run 2 | Run 3 | Unit |
|---|---|---|---|---|
| fitting run's maximum above idle | 202.0 | 202.0 | 202.0 | MiB |
| spilling run's 10 s sustained level above idle | 4,987.9 | 4,999.3 | 4,999.3 | MiB |

- **The gap:** 202.0 to 4,987.9 MiB above idle. **The margin by PF-26** (§7): twice 202.0 is 404.0, so **512 MiB**, inside the gap. **The midpoint rule** that ran in Step 3: the middle (2,595 MiB) rounded down, **2,560 MiB**, also inside the gap.
- **Against idle 1** (276 MiB, recorded): the fitting runs' maximum is 0 above it and the spilling runs' sustained level 4,785.9 MiB; PF-26 gives 128 MiB and the midpoint 2,304 MiB.
- **The adapter's `Shared Usage`** (recorded) gives the same picture: idle 185, fitting maximum 211 above it, spilling 4,983; margins 512 (PF-26) and 2,560 (midpoint).
- **What spilled:** with dynamic VRAM on, the 2048² batch-4 runs moved about 4,800 MiB into shared memory while the engine's dedicated figure stayed at or under 12,832 MiB of the 16 GB card, so this is ComfyUI placing memory in system RAM, not the driver's fallback after a full card; either way it is what APP-132 means to show.
- **Shared Usage stays up after a spill:** it rose to 5,073 MiB in the first spilling run and stayed there through the gaps and the next runs, until ComfyUI stopped. The fitting runs' 276 MiB stayed up the same way (so idle 1 equals the fitting maximum).
- **After the stop:** the script reported "something already listens on port 8199" once; a check afterwards found nothing listening and no ComfyUI process left, so it was most likely the socket still closing. The check now retries for up to 10 s before it reports (§3).

Verdict against the bar: **PASS** (every fitting run's maximum above idle, 202.0 MiB, is below every spilling run's 10 s sustained level, 4,987.9 MiB, and 512 MiB lies strictly inside).

## 6. Verdict

- **S0.1 PDH without admin: PASS.** All three English counters add and collect for the discrete GPU's LUID from a process that isn't elevated, and a collect's p99 is 1.463 ms, under 5 ms. Recorded: the instance grammar; `Shared Usage` didn't move with 1 GiB of `STAGING` buffers; the busiest engine (`3D`) and NVML agree within half a point at about 65 %.
- **S0.2 Decode cost: FAIL, on one item.** A JPEG cut inside its scan decodes as `Ok`. Every other item passes, well inside its bar: the 1024² PNG → 256 px p95 is 13.5 ms (bar 60), the 60,000² header is refused at +0.1 MiB (bar 64), the empty file and the header-cut JPEG are errors, nothing panics, and four concurrent 4096² decodes stay at +487.5 MiB (bar 1.5 GiB). The bar's FAIL path, the helper process, would return the same image, so PF-24 (§7) keeps the in-process route.
- **S0.3 Facets: PASS.** On Task 5's draft schema with SQLite's default cache, the page and six disjunctive facets take p95 69.8 ms at 10,000 assets, under 100 ms. Recorded: 980.1 ms at 50,000; with the owner's 64 MiB cache, 52.4 and 301.3 ms.
- **S0.4 Spill margin: PASS.** The fitting runs reach 202 MiB above idle and the spilling runs hold 4,988 MiB or more for 10 s, a clean gap; by PF-26 the margin is 512 MiB.

## 7. Decision

- **CORE-059 (S0.1): PDH is readable without admin.** `\GPU Adapter Memory(*)\Dedicated Usage` and `Shared Usage` and `\GPU Engine(*)\Utilization Percentage` (English names, `PdhAddEnglishCounterW`) are read by `lil-platform` from the normal, non-elevated process; Task 19 shows them rather than CORE-059's "not shown" path. The busiest engine type's sum over processes matches NVML (65.5 % against 65.0 % median under a D3D11 compute loop), so `headline_util` can come from `engines`. Instance names are `luid_<luid>_phys_<n>` and `pid_<pid>_luid_<luid>_phys_<n>_eng_<n>_engtype_<type>`, where the type may contain underscores and spaces.
- **APP-132, P4b-R5, P4b-R37 (S0.1's load part): the adapter's `Shared Usage` did not respond to 1 GiB of D3D11 `STAGING` buffers.** The adapter figure is for the Snapshot's Hardware display only; Task 19's fit signal and spill alert use the engine processes' `\GPU Process Memory(*)\Shared Usage`, as S0.4 measured it (and as P4b-R37 already says). S0.4 shows that figure responds to ComfyUI's spill (about +4,800 MiB).
- **PF-24, controller ruling, 2026-11-05 (APP-038, P4b-R12; S0.2): the decode route.** S0.2's cut-JPEG item stays FAIL as measured, but P4b-R12's in-process route stands, because the helper-process FAIL path would return the same image. Task 12 refuses a JPEG with no EOI marker and gives it the `icon@1` state.
- **Owner decision, 2026-11-05, under P4b-R23's "owner hears" (APP-138, CORE-042; S0.3): the index gets a 64 MiB SQLite page cache.** Task 5 sets `assets_query_facets_10000_ms` from a fresh measurement with that cache. Here, with the cache: p95 52.4 ms at 10,000 (twice it rounded up to 10 ms: 110 ms) and 301.3 ms at 50,000 (610 ms). Without it: 69.8 ms (140 ms) and 980.1 ms (1,970 ms).
- **PF-25, controller ruling, 2026-11-05 (S0.3): how Task 5 measures.** Task 5 adds an index on `asset_tags(tag_fold, tag)`, which covers the `tags` facet (here it read every tag row by key for `min(tag)`), and sets each facet budget from the median of three measurements, since the run-to-run spread on this desktop was large (§5).
- **PF-26, controller ruling, 2026-11-05 (APP-132, P4b-R19; S0.4): the spill margin is 512 MiB.** The bar fixes only that M lies strictly inside the gap. Among those values the margin is the smallest multiple of 128 MiB that is at least twice the fitting runs' maximum: 2 × 202 = 404, so 512 MiB. It lies inside the gap of 202 to 4,988 MiB, flags moderate spills that the midpoint (2,560 MiB) would miss, and equals P4b-R19's starting value. Task 19 keeps 512 MiB.

## 8. Follow-ups

- **Task 19 (CORE-059, APP-132, P4b-R19, P4b-R37):**
  - read `GPU Adapter Memory` and `GPU Engine` with `PdhAddEnglishCounterW` and wildcard instances, matching the discrete GPU's `luid_0x…_0x…` case-insensitively and taking everything after `engtype_` as the type;
  - the fit signal and the spill alert use the engine processes' `GPU Process Memory\Shared Usage`, never the adapter's, which didn't respond to `STAGING` buffers;
  - the margin stays 512 MiB;
  - `Shared Usage` stayed at its high level after each run until ComfyUI stopped (276 MiB after fitting runs, 5,073 MiB after a spill). P4b-R37's idle level for a later job is then that raised level, so a second spilling job in the same ComfyUI session reads as "above idle" only by its own increase. Task 19 should test that case with the fake hardware.
- **Task 12 (APP-038, P4b-R12):** the in-process route, with the EOI check of PF-24; decode, resize and encode stay well inside budget (13.5 ms p95 for 1024² → 256 px; the 4096² PNG is about 0.34 s, mostly the Lanczos3 resize).
- **Task 5 (APP-138, CORE-042, P4b-R23):** the 64 MiB page cache; the `asset_tags(tag_fold, tag)` index; budgets from the median of three measurements (PF-25). The flag facet should test takes by the key (`EXISTS`), not as a row-value `IN (SELECT …)` in the result columns, which took about 2 s at 10,000 assets.