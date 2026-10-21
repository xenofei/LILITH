---
name: "routing"
description: "Worldwright reference B: OpenRouter, NanoGPT, Hugging Face Inference Providers, other gateways, direct provider platforms, LiteLLM and media hosts (fal, Replicate, ComfyUI partner nodes). Endpoints, routing parameters, fees, example configs, and usage, cost and balance endpoints. Load before making routed or hosted API calls, logging usage, or checking provider balances and quotas."
user-invocable: false
---

# Appendix B — Open routing layer

Snapshot as of 2026-10-11. Check each service's docs before relying on a parameter.

## B1. Recommended topology

| Traffic | Where it goes | Why |
|---|---|---|
| Claude | Anthropic directly | Native prompt caching, newest models and features. Running Claude Code through OpenRouter is possible (`ANTHROPIC_BASE_URL=https://openrouter.ai/api`, `ANTHROPIC_AUTH_TOKEN=<key>`, `ANTHROPIC_API_KEY=""`), but not the default. |
| Other LLMs and media | A self-hosted LiteLLM proxy, if configured | One place for keys, fallback chains and usage tracking, with a virtual key or tag per project |
| Breadth | OpenRouter | Widest catalog, privacy and price filters |
| Cheap open-weights bulk | NanoGPT | Subscription and pay-as-you-go |
| Open models | Hugging Face Inference Providers | `:cheapest` and `:fastest` selection |
| Model-specific parameters, queues, webhooks | Media models directly via fal or Replicate, or through ComfyUI partner nodes | |

## B2. OpenRouter

**Base URLs:** `https://openrouter.ai/api/v1` (OpenAI-compatible) and `https://openrouter.ai/api` (Anthropic-compatible).

**The `provider` object:**

| Field | Values / notes |
|---|---|
| `order` | Preferred providers, in order |
| `allow_fallbacks` | Default true |
| `require_parameters` | Default false |
| `data_collection` | `allow` or `deny` |
| `zdr` | Zero data retention only |
| `only`, `ignore` | Provider include and exclude lists |
| `quantizations` | |
| `sort` | `price`, `throughput` or `latency`, or `{by, partition}` |
| `preferred_min_throughput`, `preferred_max_latency` | |
| `max_price` | `{prompt, completion, request, image}`, in USD per million tokens for token prices |
| `enforce_distillable_text` | |

- Setting `sort` or `order` turns off default load balancing.
- Regional endpoints `eu.openrouter.ai` and `us.openrouter.ai` are available on Business and Enterprise plans.

**Model fallbacks:** pass a `models: [...]` array. Models are tried in order on any error, and you're billed for the model that served the request (shown in the response's `model` field).

**Model suffixes:**
- Active: `:free`, `:batch`, `:nitro`, `:floor`, `:exacto` (better tool calling).
- Deprecated: `:thinking` (use the `reasoning` parameter), `:online` (use the `openrouter:web_search` server tool), `:extended`.

**Auto Router:** set `model: "openrouter/auto"` and restrict candidates with `plugins: [{"id": "auto-router", "allowed_models": ["anthropic/*", "..."]}]`. There's no router fee.

**Presets:** reference them as `model: "@preset/<slug>"` or `preset: "@preset/<slug>"`. Request fields override the preset with a shallow merge.

**Fees:**
- Inference has no markup.
- Credit purchases: 5.5% by card ($0.80 minimum), 5% by crypto.
- BYOK: the first $25k of list-price usage per month is free, then a 5% fee.

**Media endpoints:**

| Endpoint | Notes |
|---|---|
| `POST /api/v1/images` | Fields: `resolution`, `aspect_ratio`, `size`, `n`, `seed`, `input_references` |
| `POST /api/v1/videos` | Asynchronous: poll, or pass `callback_url`. Fields: `frame_images`, `generate_audio`. Video is not eligible for zero data retention. |
| `/api/v1/audio/speech`, `/api/v1/audio/transcriptions` | No music endpoint |

**`/api/v1/models`:**
- Fields: `id`, `context_length`, `architecture.input_modalities` and `architecture.output_modalities`, `pricing.*` (including cache read/write), `top_provider.is_moderated`, `supported_parameters`, `expiration_date`.
- Filters: `output_modalities=image|video|audio|speech|embeddings` and `supported_parameters=tools`.

**Usage accounting:** every response's `usage` includes `cost`, `cost_details.upstream_inference_cost`, cached and cache-write tokens, and reasoning tokens. Balance and key endpoints are in B9.

**Prompt caching (Anthropic models):** add `cache_control` per content block (up to 4 breakpoints) or once at the top level. `ttl: "1h"` is optional. Pricing: cache write 1.25× (5 min) or 2× (1 h); cache read 0.1×.

**Structured outputs:** use `response_format` with `json_schema`, plus `provider.require_parameters: true`.

**Guardrails per key or member:** spending limits, model and provider lists, ZDR per model group, data regions, and PII and regex filters.

## B3. NanoGPT

**Base URL:** `https://nano-gpt.com/api/v1`. Use `/api/v1legacy` if a client breaks on reasoning output.

**Endpoints:**
- `/chat/completions`, `/responses`, `/messages`.
- `/models` (`?detailed=true` adds pricing).
- `/images/generations`.
- `/audio/speech` (also serves music models) and `/audio/transcriptions`.
- `/embeddings`.
- Video, asynchronous: `POST /api/generate-video`, then poll `GET /api/video/status?requestId=`. Catalog includes Veo, Kling, Seedance, Wan, Hailuo, Runway Aleph, Vidu and PixVerse; listings can lag behind provider shutdowns.

**Pricing:**
- **PRO subscription:** $12 per month for 60M input tokens a week and 100 images a day, on included models (check the model filter). Check usage at `GET /api/subscription/v1/usage`.
- **Pay-as-you-go:** list price, no markup. Pinning a provider adds about 5%, billed as pay-as-you-go. BYOK costs 5%.
- **Payment:** card, crypto, or x402 USDC per request.

**Routing:**
- A `provider` object with `order`, `only`, `ignore`, `sort`, `quantizations`, `max_price`, `allow_fallbacks` and `require_parameters`, or the `X-Provider` header.
- Suffixes: `:floor`/`:price`, `:fast`, `:latency`, `:throughput`, `:tools`, `:caching`, `:online/<engine>`, `:memory`, `:redaction`, `:reasoning-effort/<level>`.
- There is no documented auto-router.

**Privacy:**
- No prompt or response logging by default.
- Opt-in encrypted logs are kept 1–30 days.
- Zero-data-retention routing exists, but NanoGPT doesn't audit providers' claims.
- A TEE private mode is available.

**Rate limits:** set per key per day in the dashboard.

## B4. Hugging Face Inference Providers

- **Chat:** `https://router.huggingface.co/v1`, OpenAI-compatible. Model suffixes select the provider: `:fastest` (default), `:cheapest`, `:preferred` or `:<provider>`.
- **Images, video and speech:** use `huggingface_hub.InferenceClient(provider="fal-ai", api_key=HF_TOKEN)`, e.g. `.text_to_image(prompt, model=...)`. The token needs the "Inference Providers" permission. Which providers support text-to-video: verify.
- **Pricing:** no markup; PRO accounts get $2 a month of credit.
- **Privacy:** Hugging Face doesn't store request or response bodies; logs are kept up to 30 days.

## B5. Other hosted gateways

| Gateway | Base URL / notes |
|---|---|
| Vercel AI Gateway | `https://ai-gateway.vercel.sh/v1` (also `/v1/messages`). Options under `providerOptions.gateway`: `order`, `only`, `sort` (`cost`/`ttft`/`tps`), `caching: "auto"`, `byok`, `zeroDataRetention`. No markup. |
| Cloudflare AI Gateway | Proxy with caching, rate limits, retries and dynamic routes. Unified Billing charges a 5% fee on credits. |
| Requesty | `https://router.requesty.ai/v1`. 5% markup; fallback policies; EU data residency. |
| Together AI | `https://api.together.ai/v1` |
| Fireworks AI | `https://api.fireworks.ai/inference/v1`. Zero data retention by default for open models. |
| DeepInfra | `https://api.deepinfra.com/v1/openai` |
| Novita AI | `https://api.novita.ai/openai` |
| SiliconFlow | `https://api.siliconflow.com/v1`. Strong Chinese-model coverage. |
| Chutes | `https://llm.chutes.ai/v1`. TEE compute; pay in TAO or USD. |
| Groq | `https://api.groq.com/openai/v1` |
| AIMLAPI | `https://api.aimlapi.com/v1`. Also image, video, music and 3D. |
| Eden AI | Multi-provider, including OCR and audio. |

**Direct provider platforms.** Use these for first-party features or when a router lacks a model. Base URLs vary by region, so take them from each platform's docs (verify). The environment-variable names are suggestions; keep keys in the environment or a secret manager.

| Provider | Docs | Suggested variable | Notes |
|---|---|---|---|
| Anthropic | platform.claude.com | `ANTHROPIC_API_KEY` | Used by Claude Code itself |
| OpenAI | developers.openai.com | `OPENAI_API_KEY` | Images, TTS and STT; no video API since 2026-09-24 |
| Google Gemini API | ai.google.dev | `GEMINI_API_KEY` | Nano Banana, Gemini Omni (video), Lyria, TTS |
| xAI | docs.x.ai | `XAI_API_KEY` | Grok, Grok Imagine |
| Black Forest Labs | docs.bfl.ai | `BFL_API_KEY` | FLUX.2 Pro/Flex and FLUX 3 |
| DeepSeek | api-docs.deepseek.com | `DEEPSEEK_API_KEY` | OpenAI-compatible |
| Alibaba Model Studio | alibabacloud.com/help/en/model-studio | `DASHSCOPE_API_KEY` | Qwen, Qwen-Image, Wan. Has an OpenAI-compatible mode; choose the international region. |
| Moonshot | platform.kimi.ai | `MOONSHOT_API_KEY` | Kimi K3 |
| Z.ai | docs.z.ai | `ZAI_API_KEY` | GLM, GLM-Image, CogVideoX |
| MiniMax | platform.minimax.io | `MINIMAX_API_KEY` | M3, Hailuo, Speech. The Music API is closed to new users. |
| ByteDance (BytePlus ModelArk) | byteplus.com | `ARK_API_KEY` | Seed, Seedream, Seedance. Not offered in the US. |
| Kuaishou Kling | Kling developer platform | verify | Kling 3.0 / O3 |
| Tencent Cloud | tencentcloud.com | verify | Hy LLMs, Hunyuan3D 3.x (also hosted on fal) |
| ElevenLabs | elevenlabs.io/docs | `ELEVENLABS_API_KEY` | TTS, SFX, Music (check plan terms) |

## B6. Self-hosted gateways

| Gateway | License | Notes |
|---|---|---|
| LiteLLM | MIT, except the `enterprise/` folder | Latest stable is v1.104.0 (Oct 3, 2026); pin a version and verify hashes, since 1.82.7 and 1.82.8 were malicious. Endpoints: chat, `/v1/messages`, responses, images and edits, audio, `/videos`, embeddings, rerank, batches. Routing strategies include `simple-shuffle`, `least-busy`, `latency-based-routing` and `cost-based-routing`. Fallbacks, cooldowns, spend tracking, and provider and per-deployment spend limits. Virtual keys need Postgres. SSO, audit logs and per-key guardrails are Enterprise features. |
| Portkey Gateway | MIT | Fallbacks, load balancing, retries, guardrails |
| TensorZero | Apache-2.0 | Fallbacks, A/B tests, optimization |
| Bifrost | Apache-2.0 | Some features are Enterprise-only |
| Envoy AI Gateway | Apache-2.0 | v1.0 |
| Helicone AI Gateway | Repo is inconsistent (LICENSE says GPL-3.0, README says Apache) | Verify before use |
| RouteLLM | Apache-2.0 | Research-grade strong/weak routers |

## B7. Media hosts

| Host | Notes |
|---|---|
| fal | Queue at `queue.fal.run` with a `fal_webhook` parameter. Output URLs expire, so download right away. Carries most flagship video, image, 3D and audio models. |
| Replicate | Per-second hardware or per-output pricing. |
| ComfyUI partner nodes | Prepaid credits. Run local and hosted models in one graph. Turn them off with `--disable-partner-nodes`. |
| Comfy Cloud | $20–$100 a month |
| Runware | Billed only on success |
| WaveSpeed, Segmind | |
| Modal | Per-second billing |
| RunPod Serverless | `/run` (async) and `/runsync` |
| Kie.ai | Model sourcing is unclear, so don't use it for Suno, Veo or Runway |
| PiAPI | No longer offers Midjourney or Suno |

## B8. Example configurations (check field names against your versions)

OpenRouter request with fallbacks and a price limit:

```json
{
  "models": ["deepseek/deepseek-v4.1-flash", "z-ai/glm-5.3-flash"],
  "messages": [{"role": "user", "content": "Write 40 item descriptions following the style guide below..."}],
  "provider": {
    "allow_fallbacks": true,
    "require_parameters": true,
    "sort": "price",
    "max_price": {"prompt": 1, "completion": 4}
  },
  "response_format": {
    "type": "json_schema",
    "json_schema": {"name": "items", "strict": true, "schema": {"type": "object", "properties": {"items": {"type": "array", "items": {"type": "string"}}}, "required": ["items"], "additionalProperties": false}}
  }
}
```

LiteLLM proxy for non-Claude text work:

```yaml
model_list:
  - model_name: bulk-text
    litellm_params:
      model: openrouter/deepseek/deepseek-v4.1-flash
      api_key: os.environ/OPENROUTER_API_KEY
  - model_name: bulk-text-backup
    litellm_params:
      model: openai/<nanogpt-model-id>
      api_base: https://nano-gpt.com/api/v1
      api_key: os.environ/NANOGPT_API_KEY
router_settings:
  routing_strategy: simple-shuffle
  fallbacks: [{"bulk-text": ["bulk-text-backup"]}]
  allowed_fails: 3
  cooldown_time: 30
litellm_settings:
  num_retries: 2
general_settings:
  master_key: os.environ/LITELLM_MASTER_KEY
  database_url: os.environ/DATABASE_URL   # needed for virtual keys and spend tracking
```

## B9. Usage, cost and balance endpoints

Use these to fill `registry/usage.jsonl` (Appendix I10) and to check balances when it's useful. Keys come from environment variables; never log them.

| Provider | Per-call usage and cost | Balance, credits or quota | Notes |
|---|---|---|---|
| Anthropic | Each response's `usage`: `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`. Compute cost from the prices in Appendix A1. | — | Usage and Cost Admin API (`/v1/organizations/usage_report/messages`, `/v1/organizations/cost_report`) needs an Admin key (`ANTHROPIC_ADMIN_KEY`, optional) |
| OpenAI | Each response's `usage` | — | Usage and Costs APIs (`/v1/organization/usage/...`, `/v1/organization/costs`) need an admin key (`OPENAI_ADMIN_KEY`, optional) |
| OpenRouter | `usage.cost` and token details in every response (`cost_source: provider_reported`); `GET /api/v1/generation?id=<id>` for a past call (verify) | `GET /api/v1/key`: the key's `limit`, `usage` and `limit_remaining`. `GET /api/v1/credits`: the account's `total_credits` and `total_usage` (verify). | |
| NanoGPT | Usage in each response (verify which cost fields are returned) | Balance check (`POST /api/check-balance`; verify). Subscription quota: `GET /api/subscription/v1/usage`. | |
| fal | Compute from the model's listed price and the output count or duration | Billing and usage through fal's platform API or dashboard (verify the endpoint) | Prepaid credits |
| Replicate | `metrics.predict_time` on each prediction; compute from per-second or per-output pricing | — | |
| Hugging Face | Compute from the provider's price | Inference credit use on the billing page (API: verify) | PRO includes $2 a month |

When a provider doesn't report cost, log `cost_source: computed_from_usage` or `estimated`, and reconcile later from the provider's billing data if the user asks.
