---
name: "manifest"
description: "Worldwright reference I: workspace and project folder layout, worldwright.yaml and project.yaml settings, the asset manifest JSON schema, candidate runs, sign-off and sealed versions, link syntax, status and staleness rules, library items, the job queue, the model and provider registries, the usage log (registry/usage.jsonl) and user spending caps (suite.toml). Load when setting up a workspace or project, or writing or reading a manifest, run, link, library item, queue entry, registry entry or usage line."
user-invocable: false
---

# Appendix I — Workspace, projects and schemas

Path bases:
- `registry/`, `library/`, `inbox/` and `queue/` are relative to the workspace root.
- Paths in `project.yaml` and project-level files (such as `assets/`, `recipes/`, `style/`, `exports/`) are relative to `projects/<slug>/`.
- Inside a manifest, paths that start with `v<semver>/`, `candidates/`, `prompts/` or `masks/` are relative to the asset's own folder; all other paths are relative to the project folder.

## I1. Workspace layout

```
<workspace>/
  worldwright.yaml                # workspace settings and defaults (I2)
  .worldwright/state.json         # active project, last session; {"active_project": "ember-knight"}
  .worldwright/staging/           # scratch space for in-flight work (gitignored)
  .trash/                         # files deleted through the suite land here first (gitignored)
  registry/models.yaml            # models, files, hashes, settings, last_verified, fallbacks (I9)
  registry/providers.yaml         # hosted providers, endpoints, billing basis, balance checks (I9)
  registry/usage.jsonl            # every external or paid API call, one JSON object per line (I10)
  library/<kind>/<item_id>/       # promoted reusable items with manifests (I7)
  inbox/<import_id>/              # quarantine for imported files, one directory each
  queue/jobs.jsonl                # asynchronous and long-running jobs (I8)
  projects/sketchbook/            # default media project for one-off work
  projects/<slug>/                # one folder per project (I3)
```

Library kinds: `style-kits`, `palettes`, `characters`, `props`, `materials`, `audio`, `rigs`, `motions`, `fonts`, `ui-kits`, `loras`, `recipes`.

Keep the workspace under git, with LFS (or the configured VCS) for binaries. `.gitignore` covers `inbox/*`, `.worldwright/staging/`, `.trash/` and `projects/*/assets/*/candidates/`. A project shared with others can be its own repository inside `projects/`.

Outside the workspace, the user-level file `%APPDATA%\Worldwright\suite.toml` holds the user's spending caps (I10). It belongs to the user; agents read it and never write it.

## I2. Workspace settings (`worldwright.yaml`)

```yaml
name: My Worldwright
created: 2026-10-12
defaults:                         # inherited by every project unless it overrides them
  vcs: git+lfs
  models: {}                      # default model per task, e.g. {concept: qwen-image-2512, video: seedance-2.5}
hardware: {gpu: "RTX 4090 24GB", vram_gb: 24, ram_gb: 64, detected: 2026-10-12}
routing: {litellm_proxy: null}    # e.g. http://127.0.0.1:4000 when configured
comfyui: {url: "http://127.0.0.1:8188"}
tools: {}                         # local install locations (not secrets)
```

`defaults.models` keys are task names from the routing matrix (Appendix A7) or module IDs; values are `registry/models.yaml` IDs. An empty map means "use the routing matrix".

## I3. Project layout

```
projects/<slug>/
  project.yaml                    # project settings (I4)
  progress.md                     # work log: done, next, open questions
  style/bible.md                  # art and audio direction, "never" list; versioned
  style/palette.gpl               # locked palette
  style/refs/                     # reference images
  plan/assets.csv                 # asset plan (Appendix F1)
  plan/schedule.md
  assets/<asset_id>/<asset_id>.manifest.json
  assets/<asset_id>/candidates/<run_id>/...   # generation runs (gitignored)
  assets/<asset_id>/v<semver>/...             # sealed signed-off versions, with manifest.snapshot.json
  assets/<asset_id>/prompts/ masks/           # per-asset recipe inputs
  recipes/                        # project-specific ComfyUI API workflows, Blender and engine scripts
  qa/<asset_id>/<version>/        # QA reports, contact sheets, turntables
  exports/<target>/...            # engine-, app- or store-ready files
  release/                        # store kit, trailers, press kit (F2)
  datasets/<name>/                # LoRA training sets plus dataset manifests
  CREDITS.md
```

Every project type uses the same layout; avatars, personal images and videos are assets like any other, with `type` set accordingly (for example `avatar_vrm`, `image`, `video`). Folders a project doesn't need can be left out.

**Legacy folders.** Asset folders created before the candidates layout may keep candidates, derived files and the chosen file together in `v1.0.0/`, with no `candidate_runs` in the manifest, and may have extra folders from older layouts. Read them as they are. Don't move or restructure their files unless the user asks; new work on such an asset starts a new run in `candidates/`.

## I4. Project settings (`project.yaml`)

```yaml
name: Ember Knight
slug: ember-knight
type: game               # game | asset-pack | vtuber | animation | audio | media | custom
status: active           # active | paused | archived
engine: godot-4.7        # godot-4.x | unity-6.x | unreal-5.x | bevy | defold | web | none
engine_project_path: ../../../ember-knight-godot   # engine repository Engine Bridge copies exports into (relative to this folder, or absolute); null if none
platforms: [pc]          # pc, console, mobile, handheld, vr, web
style_bible: style/bible.md
style_version: 2         # raise when the bible changes in a way that affects existing assets
modules: [direction, style, concept, 2d, world, 3d, materials, rig-animation, type-ui,
          audio, voice, narrative-localization, engine-bridge, release]
pipeline:
  skip_stages: []        # stage IDs: intake, plan, generate, repair, check, review, export, record, report
                         # e.g. [export] for a media project; check and record can't be skipped
  extra_reviews: []      # optional extra reviews in the review stage, e.g. [{by: user, types: [character_3d]}]
  auto_approve: []       # asset types the gates may sign off on their own
links: [lib:pal_ember, proj:ember-knight]   # sources this project may draw on; a new lib: or proj: source
                         # needs the user's OK the first time, and is then added here
vcs: git+lfs             # git+lfs | perforce | unity-vc | diversion | none
hardware: inherit        # or a profile like the workspace's, e.g. for a project rendered on another machine
created: 2026-10-12
```

Keys that are left out inherit the workspace `defaults`; a project can override any of them, including `models`. Module IDs: `direction`, `style`, `concept`, `2d`, `world`, `3d`, `materials`, `rig-animation`, `type-ui`, `audio`, `voice`, `narrative-localization`, `video`, `avatar`, `hub-imports`, `training`, `engine-bridge`, `release`. `hub-imports` and `training` are workspace-wide and always available.

## I5. Asset manifest (`<asset_id>.manifest.json`)

```json
{
  "asset_id": "chr_knight",
  "project": "ember-knight",
  "module": "3d",
  "version": "1.4.0",
  "approved_version": "1.3.2",
  "parent_version": "1.3.2",
  "type": "character_3d",
  "status": "in_review",
  "status_reason": null,
  "style_bible": "style/bible.md#knights",
  "inputs": [
    {"ref": "chr_knight_concept@1.2.0", "role": "concept"},
    {"ref": "lib:pal_ember@2.0.0", "role": "palette"},
    {"ref": "style@2", "role": "style"}
  ],
  "pinned_inputs": [],
  "recipe": [
    {"step": "concept", "tool": "comfyui", "model": "Qwen-Image-Edit-2511",
     "model_sha256": "…", "seed": 184467, "prompt_ref": "prompts/knight.txt",
     "references": ["style/refs/knight_front.png"], "graph": "recipes/concept_v2.api.json",
     "ran_on": "local"},
    {"step": "mesh", "tool": "trellis2", "model": "microsoft/TRELLIS.2-4B",
     "seed": 9921, "input": "chr_knight_concept@1.2.0", "notes": "geometry only; texture baked in Blender"},
    {"step": "retopo_lod", "tool": "blender", "version": "5.2",
     "script": "recipes/retopo_lod.py", "params": {"tri_budget": [12000, 4000, 1200]}},
    {"step": "rig", "tool": "unirig", "human_review": true}
  ],
  "candidate_runs": [
    {"run_id": "r_20261012_0931", "path": "candidates/r_20261012_0931/", "status": "open",
     "created": "2026-10-12T09:31:00Z", "target_version": "1.4.0",
     "recipe": {"step": "material:helmet.roughness", "tool": "comfyui",
                "graph": "recipes/roughness_inpaint.api.json", "mask": "masks/helmet_uv.png"},
     "files": [
       {"path": "candidates/r_20261012_0931/helmet_rough_a.png", "seed": 77, "role": "candidate"},
       {"path": "candidates/r_20261012_0931/helmet_rough_b.png", "seed": 78, "role": "candidate"},
       {"path": "candidates/r_20261012_0931/helmet_rough_b_fix.png", "seed": 78, "role": "repair",
        "from": "helmet_rough_b.png"},
       {"path": "candidates/r_20261012_0931/chr_knight.glb", "role": "derived",
        "from": "helmet_rough_b_fix.png"}
     ],
     "qa": {"chr_knight.glb": {"gltf_validator": "pass", "vision_score": 0.9}},
     "usage": {"job_ids": [], "est_cost": 0, "cost": 0, "currency": "USD"},
     "selected": null, "signed_off": null}
  ],
  "routing": [
    {"step": "concept_escalation", "via": "openrouter", "model": "…", "provider": "…",
     "job_id": "j_20261010_0003", "cost": 0.04, "currency": "USD"}
  ],
  "environment": {"gpu": "RTX 4090 24GB", "driver": "…", "cuda": "…", "torch": "2.x",
                  "comfyui": "v0.39.2", "custom_nodes": {"<id>": "<commit>"}},
  "partial_edits": [
    {"version": "1.4.0", "scope": "material:helmet.roughness", "mask": "masks/helmet_uv.png", "seed": 77}
  ],
  "repairs": [
    {"version": "1.3.1", "defect": "non_manifold_edges", "detector": "trimesh",
     "fix": "manifold3d union", "result": "pass"}
  ],
  "human_edits": [
    {"path": "v1.3.2/knight_albedo.png", "sha256": "…", "author": "alex",
     "date": "2026-10-09", "note": "hand-painted crest"}
  ],
  "locks": ["v1.3.2/knight_albedo.png"],
  "budgets": {"tris_lod0": 12000, "texture_px": 2048, "materials": 1, "bones": 54},
  "qa": {"mesh_manifold": "pass", "uv_overlap_pct": 0.0, "texel_density_ok": true,
         "gltf_validator": "pass",
         "vision_review": {"model": "claude-haiku-5-5", "score": 0.91, "notes": "…"}},
  "hashes": {"v1.3.2/chr_knight.glb": "sha256:…"},
  "exports": [{"engine": "godot-4.7", "format": "glb", "path": "exports/godot/chr_knight.glb",
               "placed_path": "<engine_project_path>/assets/characters/chr_knight.glb",
               "axis_conversion": "Z-up→Y-up", "scale": 1.0}],
  "cost_usd": 0.42,
  "created": "2026-10-11T12:00:00Z",
  "updated": "2026-10-12T09:40:00Z",
  "unverified_fields": []
}
```

`budgets` here are performance budgets (triangles, texture size), not money.

**Version fields**
- `approved_version`: the last sealed version, or `null` if nothing has been signed off yet.
- `version`: the version the next sign-off will create. When new work starts on an asset, set it by the bump rules; right after a sign-off it equals `approved_version` until new work starts.
- `parent_version`: the sealed version the current work builds on.

**Version numbers**
- Major: silhouette or style change.
- Minor: partial regeneration.
- Patch: export or QA fix.

**Candidate runs**
- Each generation run gets its own folder, `candidates/<run_id>/` (gitignored), and one entry in `candidate_runs[]`. Run IDs look like `r_<yyyymmdd>_<hhmm>`; add a suffix if two runs start in the same minute.
- A run records: `run_id`, `path`, `created`, `target_version`, the `recipe` (or the recipe step it runs), `files[]` (each with `path`, `seed` where one applies, `role` and, for `derived` and `repair` files, `from`), `qa` results, `usage` (the run's `job_ids`, `est_cost`, `cost`, `currency`) and `status`.
- File roles: `candidate` (straight from the generator), `derived` (made from another file in the run: cutouts, upscales, conversions, exports) and `repair` (a fixed version of another file).
- Run status: `open` (still in play), `selected` (a file from it was signed off) or `rejected`.
- Runs are never edited after sign-off, apart from their status and `trashed_utc`, which the suite sets when it moves a rejected run's folder to trash (plan 2b clarification: spec Appendix E).

**Sign-off**
- The user, the suite's assistant or Claude Code may sign off. There's no approval gate; record who did it.
- Steps:
  1. Move the chosen file or files from the run into a new `v<version>/` folder. Copy instead of moving when other files in the run were derived from them.
  2. Record their hashes under `hashes`.
  3. Set `approved_version` to `version`, set `status: approved`, set the run's `status: selected`, `selected` (the chosen file names) and `signed_off: {"by": "user" | "assistant" | "claude-code" | "cli", "date": "…", "version": "1.4.0"}`. Mark other open runs for that version `rejected` unless the user wants to keep them open.
  4. Write `v<version>/manifest.snapshot.json`: a copy of the manifest as it stands after step 3.
- From then on the `v<version>/` folder is sealed: never write into it again. Fixes and changes go into a new run and a new version.

**Secrets**
- Never store API keys, tokens or personal data in a manifest.

## I6. Links, status and staleness

**Link syntax** (used in `inputs`, `pinned_inputs`, recipe steps and `links`):

| Form | Points to |
|---|---|
| `<asset_id>@<version>` | An asset in the same project |
| `lib:<item_id>@<version>` | A library item |
| `proj:<slug>/<asset_id>@<version>` | An asset in another project |
| `style@<n>` | The project's style bible at version n |

In `project.yaml` `links`, list sources without versions: `lib:<item_id>` or `proj:<slug>`. `links` only declares which sources a project may use; dependencies are always found from the manifests' `inputs`.

**Status values:** `planned`, `in_progress`, `candidate`, `in_review`, `approved` (signed off), `exported`, `released`, plus the side states `stale`, `blocked` and `deprecated`. For `blocked` and `stale`, set `status_reason` (for example `"input chr_knight_concept 1.3.0 signed off"` or `"waiting for the user to pick a palette"`).

**Staleness rule.** An asset is stale when:
- an input's `approved_version` is now higher than the version in its ref, and that input isn't listed in `pinned_inputs`; or
- the project's `style_version` is higher than the N in the asset's `style@N` input.

To find dependents, scan manifests for refs to the changed asset: in its own project for a plain ref; in every project for `lib:` refs to a library item and for `proj:<slug>/` refs to an asset in another project. Archived projects are skipped until restored. Process updates in topological order of the graph; a cycle is an error to report, not to resolve silently.

**Pinning.** `"pinned_inputs": [{"ref": "chr_knight_concept@1.2.0", "reason": "keeps the original silhouette", "by": "user", "date": "2026-10-12"}]`.

**Detached transfers** record their source in `origin` instead of `inputs`, so they never go stale: `"origin": {"project": "sketchbook", "asset_id": "img_lighthouse", "version": "1.0.0", "transferred": "2026-10-12"}`.

## I7. Library items

Each item lives at `library/<kind>/<item_id>/` with its versioned files and a manifest in the I5 schema plus:

```json
"origin": {"project": "ember-knight", "asset_id": "chr_knight", "version": "1.4.0", "promoted": "2026-10-12"},
"library": {"kind": "characters", "tags": ["knight", "stylized"]}
```

Who uses an item isn't stored on the item; find it by scanning manifests for `lib:` refs (I6). Item IDs are unique across the library; if a promoted asset's ID is already taken by an item from another project, prefix it with the project slug (`ember-knight_chr_knight`). Promoting a later version of the same asset adds that version to the existing item.

Promotion copies a sealed version; it never moves it. The manifest, recipe and inputs are carried over unchanged. Candidate runs aren't promoted.

## I8. Job queue (`queue/jobs.jsonl`)

One JSON object per line; append a new line for every state change, and read the latest line per `job_id`.

```json
{"job_id": "j_20261012_0007", "project": "ember-knight", "asset_id": "vid_teaser", "module": "video",
 "run_id": "r_20261012_0120", "step": "generate", "provider": "fal", "model": "…", "external_id": "…",
 "status": "running", "substatus": "rendering", "est_cost_usd": 3.20,
 "submitted": "2026-10-12T01:20:00Z", "updated": "2026-10-12T01:24:00Z",
 "outputs": [], "error": null}
```

Status values: `queued`, `running`, `succeeded`, `failed`, `cancelled`, `downloaded`. The optional `substatus` is free text that adds detail without changing the status, for example `waiting_for_gpu`, `rendering`, `polling`, `downloading` or `retry_2`. Before resubmitting a job, check its status with the provider using `external_id`. Download outputs into the job's run folder as soon as it succeeds, since many hosts expire output URLs. Every paid job also gets a line in the usage log (I10) with the same `job_id`.

## I9. Registry entries

`registry/models.yaml`:

```yaml
- id: qwen-image-2512
  kind: image
  source: hf:Qwen/Qwen-Image-2512
  files:
    - {path: ComfyUI/models/diffusion_models/qwen_image_2512.safetensors, sha256: "…"}
  base_family: qwen-image
  license: Apache-2.0             # for reference only
  recommended: {steps: "…", cfg: "…", sampler: "…"}
  trigger_words: []
  vram_gb_min: "…"
  price: null                     # hosted models: e.g. {per_image: 0.03, currency: USD} or {input_per_mtok: 1, output_per_mtok: 4}
  last_verified: 2026-10-11
  fallbacks: [z-image, flux2-klein-4b]
```

`registry/providers.yaml`:

```yaml
- id: openrouter
  base_url: https://openrouter.ai/api/v1
  key_env: OPENROUTER_API_KEY     # name of the environment variable, never the key
  billing_basis: prepaid_credits  # payg | prepaid_credits | subscription_quota | free_tier | byok | local
  reports_cost: true              # responses include the call's cost
  balance_check: "GET /api/v1/key"
  last_verified: 2026-10-11
```

## I10. Usage log (`registry/usage.jsonl`) and spending caps

**Usage log.** Append one JSON object per line for every external or paid API call; local runs may be logged with `billing_basis: local`. Never rewrite earlier lines. To correct a cost, append a new line with the same `job_id` and `cost_source: reconciled`; the latest line per `job_id` wins.

| Field | Meaning |
|---|---|
| `ts` | ISO 8601 UTC timestamp of the call |
| `workspace` | `name` from `worldwright.yaml` |
| `project`, `asset` | Project slug and asset ID; `null` for calls outside an asset (for example registry refreshes) |
| `job_id` | The queue job ID, or a generated ID for synchronous calls (`c_<yyyymmdd>_<hhmmss>_<n>`) |
| `tool` | What made the call: `claude-code`, `comfyui`, `litellm`, `curl`, an MCP tool name, a script path |
| `provider` | Who billed it: `anthropic`, `openrouter`, `nanogpt`, `fal`, `replicate`, `hf`, `openai`, … |
| `model` | Model ID as sent to the provider |
| `route` | How it was reached: `direct`, `openrouter`, `nanogpt`, `litellm`, `hf-inference`, `comfy-partner`, `local` |
| `billing_basis` | `payg`, `prepaid_credits`, `subscription_quota`, `free_tier`, `byok` or `local` |
| Usage units | Whichever apply: `input_tokens`, `output_tokens`, `cached_tokens`, `reasoning_tokens`, `images`, `seconds`, `characters`; omit the rest |
| `est_cost` | Estimate made before the call |
| `cost` | Actual cost, or `null` until known |
| `cost_source` | `provider_reported`, `computed_from_usage`, `estimated` or `reconciled` |
| `currency` | Usually `USD` |
| `latency_ms` | Wall-clock time from request to final result |
| `status` | `succeeded`, `failed`, `cancelled` or `pending` |

Examples:

```json
{"ts": "2026-10-12T14:03:11Z", "workspace": "Worldwright", "project": "ember-knight", "asset": "itm_descriptions", "job_id": "c_20261012_140311_1", "tool": "litellm", "provider": "openrouter", "model": "deepseek/deepseek-v4.1-flash", "route": "openrouter", "billing_basis": "prepaid_credits", "input_tokens": 18240, "output_tokens": 6120, "cached_tokens": 0, "reasoning_tokens": 0, "est_cost": 0.01, "cost": 0.0087, "cost_source": "provider_reported", "currency": "USD", "latency_ms": 9400, "status": "succeeded"}
{"ts": "2026-10-12T15:20:42Z", "workspace": "Worldwright", "project": "sketchbook", "asset": "vid_lighthouse", "job_id": "j_20261012_0007", "tool": "curl", "provider": "fal", "model": "bytedance/seedance-2.5/text-to-video", "route": "direct", "billing_basis": "prepaid_credits", "seconds": 15, "est_cost": 1.80, "cost": 1.80, "cost_source": "computed_from_usage", "currency": "USD", "latency_ms": 184000, "status": "succeeded"}
```

**Month-to-date spend** = the sum of `cost` (or `est_cost` where `cost` is `null`) over the latest line per `job_id` with a `ts` in the current calendar month, grouped by project and provider as needed.

**Spending caps (`%APPDATA%\Worldwright\suite.toml`).** Optional and set by the user only. Agents read the file and never create, edit or work around it. No file, or no `[caps]` table, means no caps. Format:

```toml
[caps]
monthly_usd = 50              # all providers together, per calendar month
per_call_max_usd = 2          # optional: any single call or job

[caps.provider_monthly_usd]   # optional per-provider monthly caps
openrouter = 20
fal = 15
```

Before a paid call or batch: if month-to-date plus the estimate would exceed `monthly_usd` or that provider's monthly cap, or a single call's estimate exceeds `per_call_max_usd`, stop and ask the user. Their yes covers that call or batch only and doesn't change the file.
