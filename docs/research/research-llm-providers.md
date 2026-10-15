# LLM provider layer research (2026-10-12)

**Status:** Only Anthropic, OpenRouter, NanoGPT and fal are verified against primary docs in this file. Four parallel research agents were still running when the run had to end, so their results are missing. They covered: OpenAI, Gemini, xAI, Mistral, DeepSeek, Alibaba DashScope, Moonshot/Kimi, Z.ai, MiniMax, BytePlus, Tencent, Xiaomi MiMo and StepFun. Rows for those providers come from prior knowledge, are marked **UNVERIFIED**, and must be checked against the docs.

## Comparison table

| Provider | OpenAI chat | Responses | Anthropic /messages | Models list has prices | Cost in response | Balance / usage API | Rate-limit headers |
|---|---|---|---|---|---|---|---|
| Anthropic | compat layer only (test-grade) | no | **native** | no (UNVERIFIED) | no | Admin key: usage_report + cost_report (orgs only; no balance) | `anthropic-ratelimit-*` |
| OpenRouter | yes | yes (GA) | yes | **yes** (USD/token) | **`usage.cost`** | `/api/v1/key` (any key); `/api/v1/credits` (mgmt key); `/api/v1/generation?id=` | only on 429 (`X-RateLimit-*`) |
| NanoGPT | yes | yes | yes | yes (`detailed=true`, USD/M) | no (24 h lookup by `X-Request-ID`) | check-balance, v1/usage, subscription usage, request billing | `Retry-After` only |
| fal | via OpenRouter proxy | via proxy | no | pricing API | n/a | `/v1/account/billing?expand=credits` | not checked |
| OpenAI | native | native | no | no | no | Admin key `/v1/organization/usage/*`, `/costs` (UNVERIFIED) | `x-ratelimit-*` (UNVERIFIED) |
| Gemini | compat `/v1beta/openai/` (partial) | no (UNVERIFIED) | no | no | no | none known (UNVERIFIED) | none known |
| DeepSeek | yes | no | yes (`/anthropic`) (UNVERIFIED) | no | no | `GET /user/balance` (UNVERIFIED) | none documented (UNVERIFIED) |
| Alibaba DashScope intl | yes (compatible-mode) | partial? (UNVERIFIED) | yes? (UNVERIFIED) | no | no | none known | UNVERIFIED |
| Moonshot/Kimi | yes | no | yes (`/anthropic`) (UNVERIFIED) | no | no | `GET /v1/users/me/balance` (UNVERIFIED) | UNVERIFIED |
| Z.ai GLM | yes (`/api/paas/v4`) | no | yes (`/api/anthropic`) (UNVERIFIED) | no | no | none known (UNVERIFIED) | UNVERIFIED |
| MiniMax | yes | no | yes (UNVERIFIED) | no | no | UNVERIFIED | UNVERIFIED |
| BytePlus ModelArk | yes (`/api/v3`) | yes (UNVERIFIED) | UNVERIFIED | no | no | UNVERIFIED | UNVERIFIED |
| Tencent Hunyuan | OpenAI-compat (China); intl is TC3-signed (UNVERIFIED) | no | UNVERIFIED | no | no | Tencent Cloud billing API (UNVERIFIED) | UNVERIFIED |
| Xiaomi MiMo | yes (UNVERIFIED) | UNVERIFIED | yes (UNVERIFIED) | no | no | UNVERIFIED | UNVERIFIED |
| StepFun | yes (UNVERIFIED) | no | UNVERIFIED | no | no | UNVERIFIED | UNVERIFIED |
| xAI | yes | yes (UNVERIFIED) | yes (UNVERIFIED) | UNVERIFIED | UNVERIFIED | management API (UNVERIFIED) | UNVERIFIED |
| Mistral | yes-ish (native, OpenAI-like) | no | no | no | no | UNVERIFIED | UNVERIFIED |

## Verified per-provider notes

### Anthropic (native)
- **Base:** `https://api.anthropic.com/v1/messages` (native). Also an OpenAI-SDK compat layer at `https://api.anthropic.com/v1/` (chat/completions). Anthropic says the compat layer is "primarily intended to test and compare" and is not production-grade.
- **Compat-layer gaps:** `strict` is ignored, `response_format` is ignored, prompt caching is not supported, and thinking is not returned. `usage.prompt_tokens_details` and `completion_tokens_details` are always empty. `parallel_tool_calls` and `stream_options` are supported. **So use the native adapter.**
- **Auth:** `x-api-key` plus `anthropic-version: 2023-06-01`.
- **Usage (native):** `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`. Total input = sum of the three input fields. No cost is returned.
- **Account APIs:** `GET /v1/organizations/usage_report/messages` (buckets of 1m, 1h, or 1d) and `GET /v1/organizations/cost_report` (daily, USD). Both need an **Admin key** (`sk-ant-admin01-…`). **The Admin API is unavailable for individual accounts**, so you must set up an organization. There is no credit-balance endpoint. Data lags by about 5 minutes.
- **Rate-limit headers:** `anthropic-ratelimit-{requests,tokens,input-tokens,output-tokens}-{limit,remaining,reset}` (reset is RFC 3339) and `retry-after`. The spend-cap 429 has no `retry-after` and carries `error.details.error_code = enforced_spend_limit_reached`.
- Sources: https://platform.claude.com/docs/en/api/openai-sdk, https://platform.claude.com/docs/en/api/usage-cost-api, https://platform.claude.com/docs/en/api/rate-limits
### OpenRouter
- **API styles (base `https://openrouter.ai/api/v1`):**
  - OpenAI chat completions (`/chat/completions`).
  - Responses API (`/responses`). The changelog says it is now GA; some reference pages still say "beta".
  - Anthropic Messages "skin" (`/messages`). Anthropic SDK base `https://openrouter.ai/api`. Claude Code support is guaranteed only for Anthropic first-party models.
- **Auth:** `Authorization: Bearer <key>`.
- **Models:** `GET /api/v1/models` **includes pricing**, as strings in **USD per token**:
  - Base prices: `prompt`, `completion`, `request`, `image`, `image_output`.
  - Cache prices: `input_cache_read`, `input_cache_write`, `input_cache_write_1h`.
  - Other: `internal_reasoning`, `web_search`, `discount`.
  - `overrides[]`: conditional prices keyed on `min_prompt_tokens`, `utc_start`/`utc_end`, or `utc_days`. **This means time-of-day pricing can appear here.**
  - Also returns `supported_parameters` (e.g. `tools`, `parallel_tool_calls`, `structured_outputs`, `response_format`, `reasoning`, `include_reasoning`). Price query filters use $/M while the response is per token.
- **Usage (always included now):**
  - `usage.cost` (credits/USD charged) and `cost_details.upstream_inference_cost`.
  - `prompt_tokens_details.cached_tokens`, `prompt_tokens_details.cache_write_tokens`, `completion_tokens_details.reasoning_tokens`.
  - When streaming, usage arrives in the final SSE chunk.
  - `usage:{include:true}` and `stream_options.include_usage` are **deprecated no-ops**.
- **Reasoning:** request `reasoning:{effort|max_tokens|exclude|enabled}`. Responses carry `message.reasoning` (string) and `reasoning_details[]` (stream: `delta.reasoning_details`). Pass `reasoning_details` back unmodified across tool calls.
- **Account APIs:**
  - `GET /api/v1/key` (ordinary API key). Returns: `limit`, `limit_remaining`, `limit_reset`, `usage`, `usage_daily`, `usage_weekly`, `usage_monthly`, the `byok_usage*` fields, `is_free_tier`, `free_model_daily_requests`, `budgets[]`, `effective_budget`. The `rate_limit` object is deprecated.
  - `GET /api/v1/credits` requires a **management key** (403 otherwise). Returns `total_credits` and `total_usage`; balance = difference.
  - `GET /api/v1/generation?id=gen-...` (normal key). Returns:
    - Cost: `total_cost`, `cache_discount`, `upstream_inference_cost` (only for BYOK).
    - Token counts: `tokens_prompt`, `tokens_completion`, `native_tokens_prompt`, `native_tokens_completion`, `native_tokens_reasoning`, `native_tokens_cached`, `native_tokens_completion_images`.
    - Request info: `provider_name`, `latency`, `generation_time`, `finish_reason`, `native_finish_reason`, `is_byok`, `streamed`, `cancelled`, `service_tier`, `api_type`, `usage`, `num_search_results`, and more.
- **Media:**
  - **`POST /api/v1/images`** (sync or streaming). `usage.cost` is in the response or in the `image_generation.completed` event. Discovery: `GET /api/v1/images/models` and `/api/v1/images/models/{author}/{model}/endpoints` (price lines `unit` = image|megapixel|token, `cost_usd`).
  - **`POST /api/v1/videos`** (async, 202 + `polling_url`). Poll `GET /api/v1/videos/{jobId}`; when the job completes it carries `usage.cost` and `usage.is_byok` plus `generation_id`. Download with `GET /api/v1/videos/{jobId}/content?index=0`. Models: `GET /api/v1/videos/models` (`pricing_skus`, e.g. per-video-second). Webhook `video.generation.completed` with HMAC header `X-OpenRouter-Signature`.
- **Rate limits:** free `:free` models allow 20 RPM, and 50/day (1000/day after $10 purchased). `X-RateLimit-Limit/-Remaining/-Reset` appear **only on 429s**, not on success, and `Retry-After` appears when upstreams give a hint. A rate limit mid-stream arrives as an SSE chunk with `finish_reason:"error"`.
- Sources:
  - https://openrouter.ai/docs/api/reference/limits
  - https://openrouter.ai/docs/use-cases/usage-accounting
  - https://openrouter.ai/docs/api/api-reference/generations/get-generation
  - https://openrouter.ai/docs/api/api-reference/credits/get-credits
  - https://openrouter.ai/docs/api/api-reference/models/get-models
  - https://openrouter.ai/docs/guides/overview/multimodal/image-generation
  - https://openrouter.ai/docs/guides/overview/multimodal/video-generation
  - https://openrouter.ai/docs/api/api-reference/anthropic-messages/create-a-message
  - https://openrouter.ai/docs/changelog
  - https://openrouter.ai/docs/use-cases/reasoning-tokens

### NanoGPT
- **Bases:**
  - `https://nano-gpt.com/api/v1`: recommended for OpenAI clients.
  - `https://api.nano-gpt.com/api/v1`: direct host for big or long requests; its catalog may differ.
  - Subscription-only: `https://nano-gpt.com/api/subscription/v1/...`
  - Paid-only model list: `/api/paid/v1/models`.
  - Reasoning variants: `/api/v1legacy/chat/completions` returns `reasoning_content`; `/api/v1thinking/...` puts reasoning inline in content.
- **API styles:** chat completions; **Responses** (`/v1/responses`, with `previous_response_id`, background mode, and built-in tools); **Anthropic Messages** (`/api/v1/messages`, Anthropic SDK base `https://nano-gpt.com/api`; any model, with Claude fully native); legacy completions.
- **Auth:** `Authorization: Bearer` or `x-api-key` on most endpoints. **`/api/check-balance` documents only `x-api-key`.**
- **Models:** `GET /api/v1/models?detailed=true` gives `pricing.prompt` and `pricing.completion` in **USD per million**, plus `pricing.currency="USD"` and `pricing.unit`. An example also shows `cacheReadInputPer1kTokens` and `cacheWriteInputPer1kTokens`, which are not in the field table (UNVERIFIED). Pricing becomes user-specific when you send a key.
- **Features:**
  - `tools` (function only, 200 KB cap) and `parallel_tool_calls`, which is forwarded to providers that support it.
  - `response_format` `json_schema` with `strict`.
  - `stream_options.include_usage`. For non-streaming calls, top-level `include_usage: true`.
  - Reasoning appears as `message.reasoning` and `reasoning_details` (stream: `delta.reasoning`). Opt into `reasoning_content` via `reasoning.delta_field` or the `v1legacy` path. Top-level `reasoning_effort` takes precedence over `reasoning.effort`.
  - Caching: `cache_control` (5m/1h, max 4 breakpoints), a `prompt_caching` helper, and `stickyprovider`.
- **Usage fields (chat):** `prompt_tokens_details.cached_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`, `completion_tokens_details.reasoning_tokens`, and top-level `reasoning_tokens`. **No `cost` field in standard chat usage.** It exists only for "decision"/Advisor models (`usage.cost` / `estimated_cost`). Responses usage uses `input_tokens_details.cached_tokens` and `output_tokens_details.reasoning_tokens`.
- **Per-request cost:** `GET https://api.nano-gpt.com/api/v1/usage/requests/{request_id}`.
  - `request_id` is the **`X-Request-ID` response header**, not the body `id`. Capture it before reading the SSE stream.
  - Use the same inference key (Bearer or x-api-key).
  - Returns `cost` (decimal string), `currency` (USD|XNO), `cost_scope`, and `usage{input_tokens, output_tokens, reasoning_tokens}`.
  - **The 24 h rolling window** means any later lookup returns 404, and 404 is not the same as zero cost.
- **Account APIs:**
  - `POST /api/check-balance` (x-api-key). Returns `usd_balance`, `nano_balance`, `nanoDepositAddress`.
  - `GET /api/v1/usage?from&to&group_by` (inference key). Per-key aggregates for the last 30 days by default, up to 366 days: `costUsd`, `refundedUsd`, `netCostUsd`, and token counts by day and/or model.
  - `GET /api/subscription/v1/usage` (alias `/api/v1/subscription/usage`; inference key). Returns `dailyInputTokens`, `weeklyInputTokens`, and `dailyImages`, each `{used, remaining, percentUsed, resetAt(ms), degraded}`, plus `limits`, `state`, and `routing.recommendedMode` (subscription|paygo|unavailable). Management tokens use `/api/management/v1/subscription/usage` instead.
- **Billing:** USD or XNO (Nano). No per-query or deposit fees documented; markup not documented.
- **Rate limits:** 25 req/s global, plus optional per-key daily RPD and USD caps that reset at 00:00 UTC (`daily_rpd_limit_exceeded`, `daily_usd_limit_exceeded`). On 429 it sends `Retry-After`. **No `X-RateLimit-*` headers documented.**
- Sources:
  - https://docs.nano-gpt.com/llms.txt
  - https://docs.nano-gpt.com/api-reference/endpoint/check-balance.md
  - https://docs.nano-gpt.com/api-reference/endpoint/usage.md
  - https://docs.nano-gpt.com/api-reference/endpoint/request-billing.md
  - https://docs.nano-gpt.com/api-reference/endpoint/subscription-usage.md
  - https://docs.nano-gpt.com/api-reference/endpoint/chat-completion.md
  - https://docs.nano-gpt.com/api-reference/endpoint/models.md
  - https://docs.nano-gpt.com/api-reference/endpoint/messages.md
  - https://docs.nano-gpt.com/api-reference/endpoint/responses.md
  - https://docs.nano-gpt.com/api-reference/miscellaneous/rate-limits

### fal (media; LLMs via an OpenRouter proxy)
- LLMs are served only through fal's OpenRouter-backed `https://fal.run/openrouter/router/openai/v1` (chat/completions, responses, embeddings) with `Authorization: Key $FAL_KEY`. Everything else is fal's queue API, which needs its own adapter (not covered here).
- **Account APIs:** these use `Authorization: Key ...` against `https://api.fal.ai/v1`.
  - `GET /v1/account/billing?expand=credits`: `credits.current_balance` and `currency`. Needs a key with `billing:usage:read` (presets BILLING, READONLY, or FULL).
  - `GET /v1/models/usage`: usage records with `cost_total`.
  - `GET /v1/models/pricing?endpoint_id=...`: unit prices.
  - `POST /v1/models/pricing/estimate`: cost estimates.
- Sources:
  - https://fal.ai/docs/platform-apis/v1/account/billing
  - https://fal.ai/docs/platform-apis/v1/models/usage
  - https://fal.ai/models/openrouter/router/openai/v1/chat/completions/api

## Unverified notes (prior knowledge; CHECK BEFORE RELYING)
- **DeepSeek:** base `https://api.deepseek.com`. Reasoner models return `message.reasoning_content`. Usage has `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` (and reportedly `prompt_tokens_details.cached_tokens`). Billing is USD internationally and CNY in China. Current status of the off-peak discount is UNVERIFIED (it may have been discontinued). Balance: `GET /user/balance` returns `balance_infos[]{currency,total_balance,...}`.
- **Moonshot/Kimi:** base `https://api.moonshot.ai/v1` (China: `api.moonshot.cn`). Balance at `GET /v1/users/me/balance`. Usage reports `cached_tokens`. Kimi thinking models emit `reasoning_content`, which must be passed back during multi-turn tool use.
- **Gemini OpenAI compat:** base `https://generativelanguage.googleapis.com/v1beta/openai/`. Partial parameter support; thinking via `reasoning_effort` / `extra_body.google.thinking_config`. Reasoning tokens are hidden or summarized. No balance API.
- **OpenAI:** usage has `prompt_tokens_details.cached_tokens` and `completion_tokens_details.reasoning_tokens`. Admin-key endpoints `/v1/organization/usage/completions` and `/v1/organization/costs`. No balance endpoint.
- **Z.ai, MiniMax, BytePlus, Tencent, MiMo, StepFun, xAI, Mistral, DashScope:** no verified detail. Re-run the research.

## Recommendation (provisional)
1. **Adapters:**
   - Native **Anthropic** `/v1/messages` adapter. It is needed for caching, thinking, strict tools and usage, and the OpenAI compat layer drops all of these. The same adapter also serves OpenRouter `/api/v1/messages`, NanoGPT `/api/v1/messages`, and (UNVERIFIED) DeepSeek, Kimi, Z.ai and MiniMax Anthropic endpoints.
   - Generic **OpenAI chat-completions** adapter with per-provider quirk profiles, for everything else.
   - Optional native **Gemini** adapter only if you need full thinking, caching or usage detail (UNVERIFIED limits of the compat layer).
   - Media (fal queue, OpenRouter images and videos) needs separate async job adapters.
2. **Quirks for the generic adapter (verified):**
   - OpenRouter:
     - Ignore `stream_options.include_usage`; usage always arrives in the final chunk.
     - Read `usage.cost`.
     - Reasoning comes as `reasoning` / `reasoning_details`, and `reasoning_details` must be passed back.
     - A mid-stream error arrives as `finish_reason:"error"`.
   - NanoGPT:
     - Non-streaming requests need top-level `include_usage`.
     - Reasoning is in `reasoning`, not `reasoning_content`, unless you use the `v1legacy` path.
     - Cache usage can come as `cache_read_input_tokens` / `cache_creation_input_tokens` alongside `cached_tokens`.
     - There is no inline cost. Capture the `X-Request-ID` header and look the cost up within 24 h.
   - Normalize cached-token fields across `prompt_tokens_details.cached_tokens`, `cache_read_input_tokens` and `prompt_cache_hit_tokens` (DeepSeek, UNVERIFIED).
   - Compute cost locally from a price table whenever the provider returns no cost. OpenRouter `overrides[]` shows that time-of-day and tiered pricing exist.
3. **Account / balance APIs:**
   - Verified:
     - OpenRouter: `/key`, `/credits` (mgmt key) and `/generation`.
     - NanoGPT: check-balance, usage, subscription usage and request billing.
     - fal: account/billing.
     - Anthropic: usage and cost reports (Admin key, orgs only, no balance).
   - Unverified: DeepSeek `/user/balance`, Kimi `/v1/users/me/balance`, OpenAI admin usage and costs.
