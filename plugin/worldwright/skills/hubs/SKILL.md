---
name: "hubs"
description: "Worldwright reference E: Civitai and Hugging Face APIs, MCP servers and CLIs, ComfyUI model folders and workflow import, model-file identification and conversion. Load when searching for, downloading, importing or identifying models, LoRAs, embeddings or workflows."
user-invocable: false
---

# Appendix E — Model hubs and custom imports

Snapshot as of 2026-10-11.

## E1. Civitai

**Sites**
- Since April 2026, civitai.com shows only part of the catalog; civitai.red hosts the full catalog.
- civitai.green redirects to civitai.com.
- The API stays on `https://civitai.com/api/v1/`.
- Responses may be silently filtered in some regions. The UK has been blocked since July 2025 (reported, not officially confirmed).

**Docs**
- developer.civitai.com. The old GitHub wiki is deprecated.

**Authentication**
- Send `Authorization: Bearer <key>`.
- `?token=` exists only for download tools, and it leaks into logs.
- Downloading any model file requires a token.

**Endpoints**

| Method | Endpoint | Notes |
|---|---|---|
| GET | `/models`, `/models/{id}` | |
| GET | `/model-versions/{id}`, `/model-versions/mini/{id}` | |
| GET | `/model-versions/by-hash/{hash}` | Accepts AutoV1, AutoV2, AutoV3, SHA256, CRC32 or BLAKE3 |
| POST | `/model-versions/by-hash` | Up to 100 SHA256 hashes |
| POST | `/model-versions/by-hash/ids` | Up to 10,000 hashes |
| GET | `/images` | |
| GET | `/tags`, `/creators`, `/enums` | `/enums` returns the current ModelType and BaseModel lists |
| GET | `/me` | Requires a token |

**Downloads**
- Fetch `https://civitai.com/api/download/models/{versionId}`. The filename comes from the Content-Disposition header.
- Prefer the official CLI:
  - Install `@civitai/cli` (binary `civitai`).
  - Search with `civitai models search --query … --type LORA --base-model "Illustrious"`.
  - Download with `civitai download <versionId> --layout comfyui --root <ComfyUI dir>`. It verifies the SHA256.
  - Authenticate with `civitai login` or the `CIVITAI_TOKEN` environment variable.

**`/models` parameters**

| Parameter | Values |
|---|---|
| `limit` | 1–100 |
| `cursor` | Use `metadata.nextCursor`. Don't use `page` together with `query`; a deep page offset returns 429. |
| `query` | Search text |
| `types` | Checkpoint, TextualInversion, Hypernetwork, AestheticGradient, LORA, LoCon, DoRA, Controlnet, Upscaler, MotionModule, VAE, Poses, Wildcards, Workflows, Detection, Other |
| `baseModels` | e.g. "SDXL 1.0", "Flux.1 D", "Flux.2 D", "Illustrious", "Pony", "Qwen", "Hunyuan Video", "Wan Video 2.2 T2V-A14B", "ZImageTurbo" |
| `sort` | Highest Rated, Most Downloaded, Most Liked, Most Discussed, Most Collected, Most Images, Newest, Oldest |
| `period` | AllTime, Year, Month, Week, Day |
| `checkpointType` | Standard, Trained, Merge |
| `primaryFileOnly` | |

**Images**
- On `/images`, `withMeta=true` returns generation parameters, including the `modelVersionId`s of the resources used.

**Fields to read**

| Level | Fields |
|---|---|
| Version | `baseModel`, `trainedWords` (trigger words), `files[]`, `downloadUrl` |
| File (`files[]` entry) | `hashes` (AutoV1/V2/V3, SHA256, CRC32, BLAKE3); `metadata.format` (SafeTensor, PickleTensor, Other); `metadata.fp`; `metadata.size`; `pickleScanResult` and `virusScanResult` (Pending, Success, Danger, Error) |
| Model | `name`, `type`, `tags`, `creator`, `modelVersions[]`, and the creator's permission flags (`allowCommercialUse`, `allowDerivatives` and others) for reference |

- Versions in Archived mode have no files. TakenDown versions also drop their images.
- Only use files whose pickle and virus scans both say "Success". Prefer the SafeTensor format.

**MCP**
- The official server is at `https://mcp.civitai.com/mcp` (Streamable HTTP). Its browse tools work without a key.
- Generation is handled by a separate "Orchestration MCP".

**Rate limits**
- No published numbers. Back off on 429 responses. Public endpoints are edge-cached for about 5 minutes.

## E2. Hugging Face Hub

**MCP**
- The official server is at `https://huggingface.co/mcp`. Add `?login` for OAuth, or send `Authorization: Bearer <HF_TOKEN>`.
- Configure tools and Gradio Spaces at huggingface.co/settings/mcp.
- Tool groups: repo search and details, Hub filesystem, dynamic Spaces, Jobs.

**Python (`huggingface_hub` 2.2)**
- `HfApi().list_models(filter=..., author=..., search=..., pipeline_tag=..., gated=..., sort="downloads"|"likes"|"trending_score"|"created_at"|"last_modified", limit=..., expand=[...])`.
  - Useful `expand` fields: `cardData`, `gated`, `safetensors`, `gguf`, `baseModels`, `childrenModelCount`, `siblings`.
- To find all LoRAs for a base model, filter on `base_model:adapter:<repo>`. Example: `https://huggingface.co/api/models?filter=base_model:adapter:black-forest-labs/FLUX.1-dev&sort=downloads`.
- Other relations: `merge`, `quantized`, `finetune`.
- Tags carry licenses, e.g. `license:apache-2.0`.

**CLI**
- `hf auth login`.
- `hf download <repo> [files] --include "*.safetensors" --local-dir ... --revision ... --dry-run`.
- `hf upload`.
- `hf cache ls | rm | prune | verify`.
- Environment variables: `HF_HOME`, `HF_TOKEN`, `HF_HUB_DISABLE_XET`. Storage uses Xet (`hf_xet`).

**Gated models**
- The user accepts the terms in a browser; approval is automatic or manual. Then authenticate with a token.

**Security**
- Pickle files get an import scan that lists their imports.
- ClamAV runs on every file but doesn't detect pickle exploits. Don't rely on either scan.

**ZeroGPU (Spaces)**
- Daily quota: 2 min unauthenticated, 5 min free, 40 min PRO, 60 min Enterprise.
- Overage costs $1 per 10 minutes.

**Inference Providers**
- See Appendix B4.

## E3. ComfyUI import

**Version**
- v0.39.2 (2026-10-08).
- Enable the Manager with `pip install -r manager_requirements.txt` and `python main.py --enable-manager`.
- Manager security levels are strong, normal, normal- and weak. Use normal or stricter. Downloads that aren't safetensors and aren't in the default list count as high risk.

**Model folders**

| Folder | Notes |
|---|---|
| `checkpoints` | |
| `diffusion_models` | Legacy name: `unet` |
| `text_encoders` | Legacy name: `clip` |
| `loras` | |
| `vae` | |
| `embeddings` | |
| `controlnet` | Also T2I adapters |
| `clip_vision` | |
| `upscale_models` | |
| `style_models`, `hypernetworks` | |
| `model_patches`, `audio_encoders` | |
| `background_removal`, `frame_interpolation` | |
| `detection` | |

- Point to other locations with `extra_model_paths.yaml` (`base_path` plus per-folder keys; an A1111 mapping template is included).
- GGUF models go in `models/unet`, loaded with ComfyUI-GGUF's Unet Loader (GGUF). Install `gguf` first.

**Workflow formats**
- UI workflow JSON v1.0 has `version`, `state`, `nodes`, `links`, `groups` and `extra`.
- Its optional `models[]` entries list `name`, `url`, `directory`, `hash` and `hash_type`. Use them to download missing models, verifying each hash.
- The API (prompt) format is what `POST /prompt` accepts. `comfy run --workflow` converts UI JSON automatically.
- Saved PNGs embed `prompt` and `workflow` text chunks.

**Dependencies** (all with `comfy-cli`)
- `comfy node deps-in-workflow --workflow=<json|png> --output=deps.json`
- `comfy node install-deps --workflow=...`
- `comfy node registry-install <id> --version <v>`
- `comfy node save-snapshot` and `restore-snapshot`
- `comfy model download --url <URL> --relative-path models/loras`. It reads the `CIVITAI_API_TOKEN` and `HF_API_TOKEN` environment variables.

**Server API**
- `/prompt`, `/queue`, `/history/{id}`, `/interrupt`, `/system_stats`, `/free`, `/object_info`, `/models/{folder}`, `/view`, `/upload/image`, and the `/ws` websocket for progress.

## E4. Identifying an unknown model file

The plugin ships `ww-model-info <file> [--json]`. It reads only the header, so no tensors are loaded. It reports:
- the metadata, including trigger words from `ss_tag_frequency`;
- the architecture, using the fingerprints below;
- the SHA256 and AutoV2 hashes.

It refuses pickle formats. Use it instead of writing ad hoc loaders.

**Read the safetensors header**
- The first 8 bytes are a little-endian u64 giving the header length N. The next N bytes are JSON; `__metadata__` holds the metadata. Read only the header; never load an unknown pickle.

**ModelSpec keys**
- `modelspec.architecture` (e.g. `stable-diffusion-xl-v1-base/lora`)
- `.implementation`, `.title`, `.resolution`
- `.trigger_phrase`, `.license`, `.hash_sha256`, `.prediction_type`, `.merged_from`

**Kohya keys**
- `ss_base_model_version`, `ss_sd_model_name`, `ss_sd_model_hash` / `ss_new_sd_model_hash`
- `ss_tag_frequency`: tag counts per dataset. The most frequent tags give the trigger words.
- `ss_network_module`, `ss_network_dim`, `ss_network_alpha`
- `ss_resolution`, `ss_output_name`, `ss_training_comment`

**Hashes**
- AutoV2 is the first 10 hex characters of the whole file's SHA256.
- For a reliable lookup, send the full SHA256 to Civitai's by-hash endpoint.

**Architecture fingerprints**

| Architecture | Tensor-key fingerprint |
|---|---|
| SD1.x / SDXL (UNet) | `input_blocks.0.0.weight`; `label_emb.0.0.weight` means SDXL-class |
| SD3 / 3.5 | `joint_blocks.0.context_block.attn.qkv.weight` |
| FLUX.1 | `double_blocks.0.img_attn.norm.key_norm.*` |
| FLUX.2 | FLUX.1 keys plus `double_stream_modulation_img.lin.weight` |
| Qwen-Image | `txt_norm.weight` |
| Wan 2.x | `head.modulation` |
| HunyuanVideo | `txt_in.individual_token_refiner.blocks.0.norm1.weight` |
| LTX-Video | `adaln_single.emb.timestep_embedder.linear_1.bias`; `audio_adaln_single` means LTX-AV |
| ACE-Step | `genre_embedder.weight` |
| Hunyuan DiT | `mlp_t5.0.weight` |

- Full checkpoints usually prefix these keys with `model.diffusion_model.`.
- Pony, Illustrious and NoobAI all share the SDXL architecture. Tell them apart with `ss_*` metadata or a Civitai hash lookup.

**Converting pickles**
- Pickle formats are executable: ask the user before installing or converting one.
- Only in a sandbox, with PyTorch 2.6 or later: `torch.load(path, weights_only=True)`, then `safetensors.torch.save_file`.
- Never set `weights_only=False` or `TORCH_FORCE_NO_WEIGHTS_ONLY_LOAD`.
- Scan the file first with ModelAudit.

## E5. Other model types

- **LLM GGUFs:**
  - Ollama: write a `Modelfile` containing `FROM /path/model.gguf`, then run `ollama create <name>`.
  - llama.cpp: `llama serve -hf <repo>-GGUF` in newer builds, or `llama-server -hf <repo>` in older ones (check `--help`).
- **Audio LoRAs:** ACE-Step 1.5 has built-in LoRA training (about 8 songs and 1 hour on a 3090). ai-toolkit and SimpleTuner also support ACE-Step.
- **RVC and TTS voice models:**
  - RVC files go in `assets/weights/*.pth` and `assets/indices/*.index`.
  - `.pth` files are pickles, so installing one needs the user's confirmation. Treat them as untrusted and scan them first.
- **3D checkpoints:** ComfyUI detects Hunyuan3D 2/2.1 and TRELLIS2 weights. Keep them in their own folders and register them like any other model.
