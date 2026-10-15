# Hosted media-generation providers: async-job, cost and recovery facts

Researched 2026-10-12, using official docs only. **UNVERIFIED** means the claim could not be confirmed on an official page. Many vendors moved their docs or shipped new API versions in 2026, so URLs and models below reflect today's state.

## Read first: changes that affect the design

- **OpenAI's Videos/Sora API was removed on 2026-09-24**, and OpenAI lists no replacement ([deprecations](https://developers.openai.com/api/docs/deprecations)).
  - `gpt-image-1` shuts down 2026-10-23.
  - `gpt-image-1-mini` and `gpt-image-1.5` shut down 2026-12-01.
  - The current models are `gpt-image-2` and `gpt-image-2.5-sunburst` / `-flare`.
- **Google now steers new work to the Interactions API** (`POST /v1beta/interactions`). It still fully supports `generateContent` but calls it "legacy".
  - **Imagen 4 is shut down.**
  - A **Gemini Omni** video model exists (`gemini-omni-1.1-flash`). There is no "Veo Omni".
- **BytePlus ModelArk (Seedance/Seedream) does not list the United States** among its supported countries ([availability](https://docs.byteplus.com/en/docs/ModelArk/availability)). Treat it as unusable for a US account, or reach the models through fal or Replicate instead.
- **Several providers have moved their docs or API versions:**
  - Tripo: v3 API on `openapi.tripo3d.com/v3`.
  - Rodin: `api.hyper3d.com/api/v2`.
  - Alibaba: per-workspace hosts `{WorkspaceId}.{region}.maas.aliyuncs.com`.
  - Kling: new task API on `api-singapore.klingai.com`.
  - MiniMax: v2 video API.
- **MiniMax music:** paid music APIs closed to new users from 2026-08-20.
- **Idempotency is almost nonexistent.** Only Kling has a usable client-supplied unique ID (`external_task_id`). Every other provider needs client-side write-ahead plus reconciliation.

---

## 1. Comparison table

Legend: **P** = poll, **W** = webhook, **S** = SSE/stream. "Cost in resp" means money or billable units come back on the call itself.

| Provider | Shape | Submit / status / cancel | Client ID / crash lookup | Output & TTL | Cost per call | Balance API | Limits |
|---|---|---|---|---|---|---|---|
| **fal** | Async queue (P/W/S) + sync `fal.run` | `POST queue.fal.run/{model}`; `GET …/requests/{id}/status`; `PUT …/cancel` | No idempotency. Recover with `GET api.fal.ai/v1/models/requests/by-endpoint?expand=payloads` | CDN URL. Default TTL conflicts across docs (≥7 d vs forever); set the `X-Fal-Object-Lifecycle-Preference` header. The queue result itself may expire after ~1 h (UNVERIFIED) | **Exact USD**: `GET /v1/models/billing-events?request_id=` (BILLING key); header `X-Fal-Billable-Units` | `GET /v1/account/billing?expand=credits` | Concurrency 2→40 (scales with spend) |
| **Replicate** | Async (P/W), `Prefer: wait` ≤60 s | `POST /v1/predictions` or `/v1/models/{o}/{n}/predictions`; `GET /v1/predictions/{id}`; `POST …/cancel`; `Cancel-After` header | None. `GET /v1/predictions?created_after=` works only for ~1 h | URL; **inputs, outputs and URLs deleted after 1 h** (API predictions) | None. Compute from `metrics.predict_time` × hardware rate, or the per-output price | None (`/v1/account` is identity only) | 600 creates/min; 3000/min for other calls |
| **OpenRouter** | Images/TTS/STT sync; **video async (P/W)** | `POST /api/v1/videos` → `GET /videos/{id}` → `/content`; no cancel | None. No job list. `GET /api/v1/generation?id=` for sync calls | Images base64; video via authenticated `/content` until `expires_at` | **`usage.cost` (USD)** in body/poll; TTS via the `X-Generation-Id` → `/generation` lookup | `GET /api/v1/key` (any key); `/credits` (management key) | Credit-based; 402 when out of credits or the in-flight budget is used up |
| **NanoGPT** | Images/speech sync; **video & ElevenLabs-TTS async (P)** | `POST /api/generate-video` → `GET /api/video/status?requestId=`; no cancel | None ("reused request ID does not deduplicate"). **`GET /api/generate-video/recover`** lists recent runs | Image URLs ~1 h (b64 default); video/audio TTL UNVERIFIED | **`cost`, `paymentSource`, `remainingBalance`** in body; `GET /api/v1/usage/requests/{X-Request-ID}` (24 h) | `POST /api/check-balance` | 25 rps; video 50/min per IP |
| **OpenAI** | Images/TTS/STT sync. Async only via Responses `background:true` or Batch | `POST /v1/images/generations` / `/edits`; `/v1/audio/speech`; **no video** | No idempotency key. `X-Client-Request-Id` is for logging only. Recover via a stored Responses `id` or Batch `custom_id` | **base64 only** (images); bytes (TTS) | Tokens in `usage` (images, STT); none for TTS. Compute locally | None. Admin-key Usage/Costs APIs (daily) | RPM/TPM/**IPM** by tier; `x-ratelimit-*` headers |
| **Gemini API** | Image/TTS/music sync (inline). **Veo = LRO (P/W)**. Interactions `background:true` | `:predictLongRunning` → `GET /v1beta/{op}` (**no Veo cancel**); `POST /interactions` → `GET`/`cancel` | None ("Job creation is not idempotent"). Operations list per model; interactions stored 55 d (paid) | Inline base64. **Veo files kept 2 days**; Files API 48 h | Tokens only (`usageMetadata` / `usage`). Compute locally | **None** (AI Studio UI / Cloud Billing) | Per-project RPM/TPM/RPD/IPM; media limits shown only in AI Studio |
| **Alibaba Model Studio** | Wan async (P); Qwen-Image sync or async | `POST …/video-synthesis` + `X-DashScope-Async: enable`; `GET /api/v1/tasks/{id}`; `POST /tasks/{id}/cancel` (PENDING only) | None. `GET /api/v1/tasks` list (24 h window) | URL **24 h**; task_id 24 h | `usage` (seconds, image count). Compute | BSS `QueryAccountBalance` (AK signing) | Wan 3.0: 5 RPS / 5 concurrent |
| **Kling** | Async (P/W) | `POST /text-to-video/kling-3.0`; `GET /tasks?task_ids=`; no cancel | **`options.external_task_id`, unique per account**; `GET /tasks?external_task_ids=`; list via `POST /tasks` (30 d) | URL; results cleared after **30 d** | **`billing[]` with `amount` and `currency`** | `GET /account/costs`; `POST /account/billing/balance` | Concurrency per resource pack; queries unlimited |
| **MiniMax** | Video async (P/W); TTS sync + async; image sync | v1: `POST /v1/video_generation` → `GET /v1/query/video_generation?task_id=` → `GET /v1/files/retrieve`; v2: `/v2/video_generation`; no cancel | None; no list | v1 File Retrieve URL **1 h** (docs conflict: 9 h); image/TTS/music 24 h; v2 re-query refreshes the URL | v2 `usage` (seconds); TTS `usage_characters`; v1 none | None (console only) | Hailuo 20 RPM; H3 300 RPM / 30 in-flight |
| **BytePlus ModelArk** | Seedream sync; Seedance async (P/W) | `POST /api/v3/contents/generations/tasks`; `GET …/{id}`; `DELETE` (queued only) | None. List tasks (7 d) | URL **24 h** (Seedance 2.5: max 100 downloads) | `usage.completion_tokens` / `generated_images` | UNVERIFIED | Seedance 2.x: 180 RPM / 3 concurrent (individual) |
| **Tencent HY 3D** | Async (P only) | TC3-signed `SubmitHunyuanTo3D{Pro,Rapid}Job` → `Query…Job` | None; no list | URL **24 h**; JobId 24 h | None. Credits from the pricing table | Billing `DescribeAccountBalance` (cash, not credits) | 20 rps; concurrency 3 (Pro) / 1 (Rapid) |
| **ElevenLabs** | **All sync** (binary/stream) | `POST /v1/text-to-speech/{voice}`; `/v1/sound-generation`; `/v1/music` | None. **TTS recoverable via `GET /v1/history`**; SFX and music are not | Response body; no URLs | **`character-cost` response header** (TTS; SFX/music UNVERIFIED) | `GET /v1/user/subscription` (quota); USD PAYG balance not exposed | Concurrency by plan (e.g. Creator 5 / Flash 10 / music 2) |
| **Tripo** | Async (P/W) | `POST /v3/generation/{type}`; `GET /v3/tasks/{id}`; batch `POST /v3/tasks/list`; no cancel | None. `GET /v3/account/usage` lists task records | Signed URL **~5 min**; re-query for a fresh one | **`credits_consumed`** (decimal; 1 credit = $0.01) | `GET /v3/account/balance` (`balance`, `frozen`) | Concurrency per category (H-series 10); `X-RateLimit-*` headers |
| **Meshy** | Async (P/S/W) | `POST /openapi/v{1,2}/{task}`; `GET …/{id}`; `/stream`; `DELETE` = cancel (PENDING only) | None. Per-type list (sorted by `-created_at`) | Signed URL until `expires_at` (**≤3 days**, non-Enterprise) | **`consumed_credits`** (charged at create; 0 on fail) | `GET /openapi/v1/balance` | 20 rps; 10–100 queued tasks per tier |
| **Rodin (Hyper3D)** | Async (P only) | `POST /api/v2/rodin` (multipart) → `POST /status {subscription_key}` → `POST /download {task_uuid}` | None. `GET /check_balance?usage_page=` lists `task_uuid` | URL TTL not stated; data kept 7 d | **`consumed`** in the submit response | `GET /check_balance` | Business plan ($120/mo) required; 120–240 RPM |

---

## 2. Per-service notes

### fal
Docs moved to `fal.ai/docs/...`. Auth header: `Authorization: Key $FAL_KEY`.

**API shape**
- Submit: `POST https://queue.fal.run/{model_id}`. Returns `request_id`, `status_url`, `response_url`, `cancel_url`, `queue_position`.
- Status: `GET …/requests/{id}/status?logs=1`. A streamed version is at `/status/stream` (SSE).
- Result: `GET …/requests/{id}`.
- Cancel: **`PUT`** `…/requests/{id}/cancel`.
  - 202 = `CANCELLATION_REQUESTED`; the request may still complete if it was already running.
  - 400 = `ALREADY_COMPLETED`.
- Statuses: `IN_QUEUE`, `IN_PROGRESS`, `COMPLETED`. **A failure is `COMPLETED` with `error`/`error_type` set.** There is no FAILED status. ([queue](https://fal.ai/docs/model-apis/model-endpoints/queue))
- Sync calls use `fal.run/{model}`, with no retries.

**Webhooks** ([webhooks](https://fal.ai/docs/model-apis/model-endpoints/webhooks))
- Enable with `?fal_webhook=<url>`. The payload carries `status` `OK`/`ERROR` and the full output.
- Signed with Ed25519. The signed message is `request_id\nuser_id\ntimestamp\nsha256(body)`, verified against the JWKS at `rest.fal.ai/.well-known/jwks.json`.
- Up to 31 retries, until the stored result expires: ~1 h, or ~6 min for results ≥10 KB.
- Duplicate deliveries are possible.
- Private/loopback targets are dropped, so a desktop app must poll.

**Useful request headers** ([common params](https://fal.ai/docs/documentation/model-apis/common-parameters))
- `X-Fal-Request-Start-Timeout`
- `X-Fal-Queue-Priority`
- `X-Fal-No-Retry`
- `X-Fal-Object-Lifecycle-Preference: {"expiration_duration_seconds":N}`
- `x-app-fal-disable-fallback: true`. Fallback to "equivalent" endpoints is on by default, which can change the price.

The response header **`X-Fal-Billable-Units`** is also documented on that page.

**Idempotency and recovery**
- No idempotency or client ID on submit.
- fal retries 503/504 internally up to 10 times, keeping the same `request_id` ([reliability](https://fal.ai/docs/documentation/model-apis/inference/reliability)).
- To recover after a crash, call `GET https://api.fal.ai/v1/models/requests/by-endpoint?endpoint_id=…&start=…&expand=payloads` and match on `json_input`. Needs scope `models:requests:read` ([by-endpoint](https://fal.ai/docs/platform-apis/v1/models/requests/by-endpoint.md)).
- Whether in-flight requests appear in that listing is UNVERIFIED.

**Output**
- Public CDN URLs on `v3.fal.media`.
- Retention: the FAQ says "at least 7 days", but the headers page says account default or forever. **These conflict.**
- Account setting: `GET /v1/storage/settings`.
- Delete: `DELETE /v1/models/requests/{id}/payloads`. This is the only fal endpoint with an `Idempotency-Key`.

**Cost**
- Exact cost: **`GET https://api.fal.ai/v1/models/billing-events?request_id=a,b`** (1–50 IDs, ≤90-day range). Returns `quantity`, `unit`, `unit_price`, `cost_total` (USD), `cost_estimate_nano_usd`. Scopes: `billing:usage:read`, `auth:keys:read`, `account:users:read` (BILLING or FULL preset). ([billing-events](https://fal.ai/docs/platform-apis/v1/models/billing-events.md), verified)
- Price list: `GET /v1/models/pricing?endpoint_id=` (any key). Returns `unit_price`, `unit`, `currency` ([pricing](https://fal.ai/docs/platform-apis/v1/models/pricing.md)).
- Estimate: `POST /v1/models/pricing/estimate` with `{estimate_type: historical_api_price|unit_price, endpoints:{id:{call_quantity|unit_quantity}}}`.
- Usage over time: `GET /v1/models/usage` (bucketed, BILLING key) ([usage](https://fal.ai/docs/platform-apis/v1/models/usage.md)).
- Units vary per model: image, megapixel, second, 1k tokens, 1k characters, generation.
- 5xx responses are never billed; a 422 may be billed.

**Balance**
- `GET https://api.fal.ai/v1/account/billing?expand=credits` → `credits.current_balance` ([billing](https://fal.ai/docs/platform-apis/v1/account/billing.md)).

**Key scopes** ([auth](https://fal.ai/docs/api-reference/platform-apis/authentication.md))
- Presets are `API`, `BILLING`, `COMPUTE`, `FULL`. Endpoint pages also mention READONLY.
- No `ADMIN` preset was found.
- **Use two keys:** an API key for inference and a BILLING key for cost and balance.

**Rate limits** ([concurrency](https://fal.ai/docs/documentation/model-apis/concurrency-limits))
- Measured as concurrent `IN_PROGRESS` requests: 2 for new accounts, rising automatically to 40 with spend.
- Queued requests don't count.
- Exceeding the limit returns 429 `concurrent_requests_limit`.

### Replicate

**API shape** ([HTTP ref](https://replicate.com/docs/reference/http), [openapi.json](https://api.replicate.com/openapi.json))
- Create a prediction with either:
  - `POST /v1/predictions {version, input, webhook, webhook_events_filter}`, or
  - `POST /v1/models/{owner}/{name}/predictions` for official models.
- `GET /v1/predictions/{id}`.
- `POST /v1/predictions/{id}/cancel`.
- `GET /v1/predictions` lists predictions, filterable with `created_after` / `created_before`.

**Request headers**
- `Prefer: wait[=1..60]` makes the call synchronous; if time runs out it returns the in-progress object.
- `Cancel-After: 5m` sets a server-side deadline (min 5 s). The prediction's `deadline` field reflects it. This guards against runaway cost if the app dies. ([create](https://replicate.com/docs/topics/predictions/create-a-prediction))

**Statuses**
- `starting`, `processing`, `succeeded`, `failed`, `canceled`, `aborted`.
- `aborted` means the deadline passed before the job started; it is not billed.
- Predictions time out after 30 min of running.

**Webhooks** ([receive](https://replicate.com/docs/topics/webhooks/receive-webhook))
- Event filter: `start`, `output`, `logs`, `completed`.
- Only the terminal event is retried, for about 1 min.
- Signed with HMAC-SHA256 over `{webhook-id}.{timestamp}.{body}`. The secret comes from `GET /v1/webhooks/default/secret`.

**Idempotency and recovery**
- No idempotency or metadata field.
- Recovery: list predictions with `created_after` and match `model` and `input`. This only works within about 1 h.

**Output**
- Files are served from `replicate.delivery`. URLs "expire after one hour".
- API-created predictions have their input, output and logs removed after 1 h (`data_removed: true`) ([retention](https://replicate.com/docs/topics/predictions/data-retention)).

**Cost**
- Nothing comes back on the call. Compute it from `metrics.predict_time` × the per-second hardware rate (e.g. `gpu-l40s` $0.000975/s), or from the per-output price for official models (e.g. flux-1.1-pro $0.04/image, wan-2.1-i2v-480p $0.09/s) ([pricing](https://replicate.com/pricing)).
- Whether `predict_time` equals the billed seconds is UNVERIFIED.
- Failed predictions are not billed. Canceled official-model runs may be.

**Account**
- `GET /v1/account` returns identity only. No balance API.

**Rate limits** ([rate limits](https://replicate.com/docs/topics/predictions/rate-limits))
- 600 creates/min; 3000/min for other endpoints.
- 429 responses carry a "resets in ~Ns" message.

### OpenRouter (`https://openrouter.ai/api/v1`)

**Images**
- `POST /api/v1/images` (sync) returns `data[].b64_json`. Supports `n`, `aspect_ratio`, `resolution`, `background`, `output_format`, and SSE on some models ([image guide](https://openrouter.ai/docs/guides/overview/multimodal/image-generation)).
- Or chat completions with `modalities:["image","text"]`, which returns `message.images[].image_url.url` as a data URL.
- Price units per model: `image`, `megapixel` or `token`.

**Video** (async; verified [video guide](https://openrouter.ai/docs/guides/overview/multimodal/video-generation))
- `POST /api/v1/videos` → 202 with `{id, polling_url, status}`.
- Poll with `GET /api/v1/videos/{id}`.
- Download with `GET /api/v1/videos/{id}/content?index=0`. **Send the API key on this request**, because `unsigned_urls` are not presigned.
- Statuses: `pending`, `in_progress`, `completed`, `failed`. Webhooks add `cancelled` and `expired`.
- **No cancel endpoint and no job list.**
- `expires_at` appears in the poll reference but not in the guide, and no TTL is given.
- Webhooks: per-request `callback_url` or a workspace default. Signed with `X-OpenRouter-Signature` (HMAC-SHA256 over `t,body`). Dedupe on `X-OpenRouter-Idempotency-Key = <job>-<status>` ([webhooks](https://openrouter.ai/docs/cookbook/video-generation/video-generation-webhooks.md)).
- Price is per video-second, listed in `pricing_skus` on `/videos/models`. `usage.cost` arrives on the completed poll.

**Audio**
- TTS: `POST /api/v1/audio/speech` (OpenAI-compatible). Returns raw bytes and an `X-Generation-Id` header. The body has no cost ([TTS](https://openrouter.ai/docs/guides/overview/multimodal/tts)).
- STT: `POST /api/v1/audio/transcriptions`. Returns `usage.cost` ([STT](https://openrouter.ai/docs/guides/overview/multimodal/stt)).
- Chat audio output requires `stream:true`.

**Cost**
- `usage.cost` (USD) is always included; `usage:{include:true}` is now a no-op ([usage accounting](https://openrouter.ai/docs/cookbook/administration/usage-accounting.md)).
- `GET /api/v1/generation?id=` returns `total_cost` and `api_type` after the fact.

**Balance**
- `GET /api/v1/key` works with any key: `limit_remaining`, `usage`, `usage_daily/weekly/monthly`.
- `GET /api/v1/credits` needs a management key ([key](https://openrouter.ai/docs/api/api-reference/api-keys/get-current-key)).

**Rate limits** ([limits](https://openrouter.ai/docs/api/reference/limits))
- Free models: 20 rpm, plus 50 or 1000 per day.
- 402 with `in_flight_budget_exhausted` and `Retry-After` means "retry later", not "out of money".
- Batch API is text-only.

**3D:** not found (UNVERIFIED).

### NanoGPT (`https://api.nano-gpt.com`)
Auth is `x-api-key` or Bearer.

**Images**
- `POST /api/v1/images` is the normalized route; its response and cost fields are undocumented.
- Legacy `POST /api/v1/images/generations` (OpenAI-compatible) returns `cost`, `paymentSource`, `remainingBalance`. Output is `b64_json` by default, or signed URLs that expire after ~1 h ([OpenAI-compat](https://docs.nano-gpt.com/api-reference/endpoint/image-generation-openai.md)).
- Edits: `/api/v1/images/edits`.
- Midjourney is async: `POST /api/check-midjourney-status`.

**Video** ([video](https://docs.nano-gpt.com/api-reference/endpoint/video-generation.md), [status](https://docs.nano-gpt.com/api-reference/endpoint/video-status-unified.md))
- `POST /api/generate-video` → 202 with `runId` and `cost` (estimate, pre-charged).
- Poll `GET /api/video/status?requestId=<runId>`. Result is at `data.output.video.url` and `data.cost`.
- **Status casing differs between docs and schema** (`COMPLETED` vs `completed`); parse case-insensitively.
- No cancel. Failures are refunded automatically.

**Recovery**
- **`GET /api/generate-video/recover?model=&limit≤50`** lists recent runs; rate-limited to 20 per minute ([recover](https://docs.nano-gpt.com/api-reference/endpoint/video-recover.md)).
- Per-request cost: `GET /api/v1/usage/requests/{X-Request-ID}`, for the last 24 h only ([request billing](https://docs.nano-gpt.com/api-reference/endpoint/request-billing.md)).

**Audio**
- `POST /api/v1/audio/speech` is sync; music models use it too.
- `POST /api/tts`: sync for OpenAI/Kokoro models (`audioUrl`, `cost`). Async for ElevenLabs models; poll `GET /api/tts/status?runId=&model=&cost=&paymentSource=`. **Echo `cost` back to get refunds on failure.** ([tts status](https://docs.nano-gpt.com/api-reference/endpoint/tts-status.md))
- STT: `/api/v1/audio/transcriptions` or `/api/transcribe`.

**Idempotency**
- None. The docs say: "a reused request ID does not deduplicate it" ([errors](https://docs.nano-gpt.com/api-reference/miscellaneous/error-handling.md)).

**Balance**
- `POST /api/check-balance` returns `usd_balance` and `nano_balance` as **strings**.
- Payments can be in USD or XNO, so store the currency with each cost record.

**Rate limits**
- 25 rps globally; video submits 50/min per IP ([rate limits](https://docs.nano-gpt.com/api-reference/miscellaneous/rate-limits.md)).

### OpenAI

**Images** (sync, optional SSE partials; [generate ref](https://developers.openai.com/api/reference/python/resources/images/methods/generate))
- `POST /v1/images/generations` and `/v1/images/edits`.
- Models: `gpt-image-2`, `gpt-image-2.5-sunburst`, `gpt-image-2.5-flare`. Older ones are deprecated (dates above).
- Parameters:
  - `background: transparent|opaque|auto`. Transparent needs png or webp, and is in preview on gpt-image-2.
  - `output_format`, `quality` (low…high; `xhigh`/`max` on 2.5), `size` (the gpt-image-2 family accepts custom W×H up to 3840×2160), `n` ≤10.
  - Edits accept up to 16 images, a `mask`, and `input_fidelity` (not on gpt-image-2).
- **Output is always base64.**
- `usage` reports input/output image tokens. The reference still says "gpt-image-1 only", and whether newer models return it is UNVERIFIED.
- Prices per 1M tokens: gpt-image-2.x output $30 and image input $8; gpt-image-1 output $40; 1-mini output $8. Batch is half price ([pricing](https://developers.openai.com/api/docs/pricing)).
- Async options:
  - The Responses API `image_generation` tool with `background:true` + `store:true`: poll `GET /v1/responses/{id}`, cancel with `POST …/cancel` ([background](https://developers.openai.com/api/docs/guides/background)).
  - The Batch API supports images, uses `custom_id`, and has a 24 h window.

**Audio**
- TTS: `POST /v1/audio/speech` with `tts-1`, `tts-1-hd`, `gpt-4o-mini-tts-*`, all removed 2027-01-06 (replacement: `gpt-realtime-2.1-mini`). Returns bytes with no usage.
  - Pricing: tts-1 $15 and tts-1-hd $30 per 1M characters; gpt-4o-mini-tts $12 per 1M audio output tokens.
- STT: `POST /v1/audio/transcriptions`. `usage` is either tokens or `{seconds}`. Whisper is $0.006/min.

**Video:** none (the Sora API was removed 2026-09-24).

**Idempotency**
- No `Idempotency-Key` is documented.
- `X-Client-Request-Id` is for logging only and cannot be used to look anything up ([overview](https://developers.openai.com/api/reference/overview)).

**Balance and usage**
- No balance endpoint.
- Admin-key-only: `/v1/organization/usage/images`, `/usage/audio_speeches`, and `/v1/organization/costs` (daily buckets). Admin keys can't call inference endpoints ([admin](https://developers.openai.com/api/docs/guides/admin-apis)).

**Rate limits** ([rate limits](https://developers.openai.com/api/docs/guides/rate-limits))
- Headers `x-ratelimit-*` and `retry-after`.
- Images per minute by tier: e.g. 2.5-sunburst is 20 on Build, 150 on Launch, 250 on Grow.

### Google Gemini API (`generativelanguage.googleapis.com`)

**Models** ([models](https://ai.google.dev/gemini-api/docs/models))
- Image: `gemini-nano-banana-2.1`, `gemini-3.1-flash-image` (Nano Banana 2), `gemini-3.1-flash-lite-image`, `gemini-3-pro-image` (Pro), `gemini-2.5-flash-image`. Docs disagree on whether 2.5-flash-image shut down 2026-10-02.
- Video: Veo 3.1 (standard/fast/lite, `-preview`), and `gemini-omni-1.1-flash`.
- Music: `lyria-3.5`, `lyria-3-clip-preview`, `lyria-realtime-exp`.
- TTS: `gemini-3.8-flash-tts`, `-flash-lite-tts`.

**Image**
- Interactions: `POST /v1beta/interactions` with `response_format:{type:"image", aspect_ratio, image_size}`.
- Or legacy: `models/{m}:generateContent` with `responseModalities:["IMAGE"]`.
- Output is inline base64 ([image guide](https://ai.google.dev/gemini-api/docs/image-generation)).

**Veo** ([veo](https://ai.google.dev/gemini-api/docs/veo))
- `POST /v1beta/models/{m}:predictLongRunning` returns an operation `name`.
- Poll `GET /v1beta/{name}` until `done`. The output is at `response.generateVideoResponse.generatedSamples[0].video.uri`.
- Download that URI with the `x-goog-api-key` header and follow redirects.
- **Stored 2 days.**
- **No cancel.** The operations list is `GET v1beta/models/{m}/operations` (from the discovery doc).

**Omni video** ([omni](https://ai.google.dev/gemini-api/docs/omni))
- Via Interactions, with `delivery:"uri"` for outputs over 4 MB.
- **The URI is returned only on create/stream; a later GET returns base64.**
- Whether Omni supports background mode is UNVERIFIED.

**Lyria and TTS**
- Lyria 3.x: Interactions with `response_format:{type:"audio"}`. MP3 (WAV on 3.5), 44.1 kHz, inline. Clip is 30 s; 3.5 makes songs of a few minutes ([music](https://ai.google.dev/gemini-api/docs/music-generation)).
- Lyria RealTime: WebSocket, 48 kHz PCM, experimental.
- TTS: Interactions with `speech_config`. WAV 24 kHz by default ([speech](https://ai.google.dev/gemini-api/docs/speech-generation)).

**Interactions lifecycle** ([ref](https://ai.google.dev/api/interactions-api))
- `background:true` (needs `store:true`), `GET /interactions/{id}`, `POST …/cancel`, `DELETE`.
- **No list endpoint.**
- Stored 55 d on paid tier, 1 d on free.
- Plain HTTP connections close at about 60 s.

**Batch** ([batch](https://ai.google.dev/gemini-api/docs/batch-api))
- `:batchGenerateContent` at 50% off; works for Nano Banana.
- A job not finished in 48 h becomes EXPIRED. Results kept 6 weeks.
- Correlate with per-request `key`. Not available for Veo.

**Webhooks** ([webhooks](https://ai.google.dev/gemini-api/docs/webhooks))
- Events include `video.generated` and `interaction.*`.
- Thin payloads; at-least-once delivery; needs a public URL.

**Files API** ([files](https://ai.google.dev/gemini-api/docs/files))
- Files kept 48 h; max 2 GB per file and 20 GB per project.

**Idempotency**
- None. The docs say "Job creation is not idempotent."

**Cost**
- Tokens only (`usageMetadata` / `usage`); no money in responses.
- Prices ([pricing](https://ai.google.dev/gemini-api/docs/pricing)):
  - Images: Nano Banana Pro $0.134 per 1K/2K image, $0.24 at 4K. Nano Banana 2.1 is $30 per 1M output tokens, about $0.034 per 1K image.
  - Veo 3.1: $0.40/s; Fast $0.10–0.30/s; Lite $0.05–0.08/s. Charged only on success.
  - Lyria: $0.08 per song, $0.04 per clip.
  - TTS 3.8 Flash: $9 per 1M audio tokens (25 tokens/s); price doubles 2027-01-01.

**Balance**
- No API. Prepaid credits are visible in the AI Studio UI.
- At a $0 balance, every key returns 402 ([billing](https://ai.google.dev/gemini-api/docs/billing)).

**Rate limits** ([rate limits](https://ai.google.dev/gemini-api/docs/rate-limits))
- Per project: RPM, TPM, RPD, and IPM for image models.
- Rolling 10-minute spend caps; exceeding them returns 429.
- Per-model media numbers appear only in AI Studio (UNVERIFIED).

### Alibaba Cloud Model Studio (Wan, Qwen-Image)

**Hosts and regions**
- Base host: `https://{WorkspaceId}.{region}.maas.aliyuncs.com`. Regions include ap-southeast-1 (Singapore) and **us-east-1 (Virginia)**.
- Legacy `dashscope-intl.aliyuncs.com` still works.
- API keys are region-specific ([Wan ref](https://www.alibabacloud.com/help/en/model-studio/wan3-video-generation-api-reference), [regions](https://www.alibabacloud.com/help/en/model-studio/regions)).
- On 2026-10-15, Virginia "US" deployment-mode workspaces change ([notice](https://www.alibabacloud.com/en/notice/product_change_change_announcement_for_model_studio_us_virginia_workspace_deployment_mode_876)).

**Wan video**
- `POST /api/v1/services/aigc/video-synthesis` with `X-DashScope-Async: enable` (required). Returns `output.task_id`.
- Poll `GET /api/v1/tasks/{id}` about every 15 s.
- Statuses: `PENDING`, `RUNNING`, `SUCCEEDED`, `FAILED`, `CANCELED`, `UNKNOWN` (expired or missing).
- Models: `wan3.0-video(-prime)`, `wan2.7-t2v/i2v`, …

**Task management** ([manage tasks](https://www.alibabacloud.com/help/en/model-studio/manage-asynchronous-tasks))
- List: `GET /api/v1/tasks?start_time&end_time&model_name&status` (≤24 h window).
- Cancel: `POST /api/v1/tasks/{id}/cancel` (PENDING only).
- 20 QPS.
- Whether these work on the international host is inferred, not shown in examples.

**Qwen-Image** ([ref](https://www.alibabacloud.com/help/en/model-studio/qwen-image-generation-and-editing-api-reference))
- Sync: `POST /api/v1/services/aigc/multimodal-generation/generation`. Recommended client timeout ≥600 s.
- Async variant: `…/image-generation/generation`.
- Also OpenAI-compatible: `/compatible-mode/v1/images/generations`.

**Output**
- Task IDs and output URLs both last **24 h**.

**Cost** ([pricing](https://www.alibabacloud.com/help/en/model-studio/model-pricing))
- `usage` carries `output_video_duration`, `SR`, or `image_count`.
- wan3.0-video: $0.05/$0.10/$0.20 per second (480P/720P/1080P), Singapore.
- qwen-image-2.1-turbo: $0.016 per image; 3.0-pro: $0.04–0.075.
- Failed tasks are not billed.

**Balance**
- BSS OpenAPI `QueryAccountBalance`, signed with AccessKey ([BSS](https://www.alibabacloud.com/help/en/user-center/developer-reference/api-bssopenapi-2017-12-14-queryaccountbalance)).

**Rate limits** ([limits](https://www.alibabacloud.com/help/en/model-studio/rate-limit))
- Wan 3.0: 5 RPS / 5 concurrent. qwen-image-3.0-pro: 5/min.

**US availability**
- A Virginia region exists. An explicit statement that US residents can sign up is UNVERIFIED.

### Kling (Kuaishou)

**Host and auth** ([auth](https://kling.ai/document-api/api/get-started/authentication.md))
- Base: `https://api-singapore.klingai.com`, for servers outside China.
- Auth: Bearer API key, or a legacy AK/SK JWT (HS256, `iss` = AK, `exp` = now + 1800).

**New API** ([3.0 omni t2v](https://kling.ai/document-api/api/video/3-0-omni/text-to-video.md))
- Submit: `POST /text-to-video/kling-3.0` with `settings` and `options:{callback_url, external_task_id}`.
- Get: `GET /tasks?task_ids=` or `?external_task_ids=`.
- List: `POST /tasks` (cursor, 30 d).
- Statuses: `submitted`, `processing`, `succeeded`, `failed`.

**Legacy API**
- Image: `/v1/images/generations` and `/{task_id|external_task_id}`.
- The legacy `/v1/videos/*` paths were not fetched directly (UNVERIFIED). They "remain available".

**Webhooks** ([callbacks](https://kling.ai/document-api/api/get-started/callbacks.md))
- Optional Standard-Webhooks HMAC signing.
- Retry policy is undocumented.

**Idempotency**
- `external_task_id` "must be unique within a single user account". **This is the only real idempotency and recovery key among all providers covered here.**

**Cancel:** not documented.

**Output**
- Results are cleared after 30 d.

**Cost**
- `billing[]` with `amount`, `currency`, `list_price` in the response.
- Video: 1 unit = $0.14. Kling 3.0 720p is 0.6 units/s ($0.084/s) ([video pricing](https://kling.ai/document-api/pricing/base/video.md)).
- Image: 1 unit = $0.0035 ([image pricing](https://kling.ai/document-api/pricing/base/image.md)).
- Purchases are non-refundable.

**Account APIs**
- `GET /account/costs` (resource packs; ≤1 QPS; 12 h lag).
- `POST /account/billing/balance` (per-task deductions) ([account](https://kling.ai/document-api/api/assets/account-usage.md)).

**Rate limits** ([concurrency](https://kling.ai/document-api/api/get-started/concurrency-rules.md))
- Concurrency is per resource pack; error 1303 when over the limit.
- Queries are not rate-limited.

**US availability:** not stated (UNVERIFIED).

### MiniMax (`https://api.minimax.io`)

**Video v1 (Hailuo 2.3/02)**
- `POST /v1/video_generation` → `GET /v1/query/video_generation?task_id=`.
- Statuses: `Preparing`, `Queueing`, `Processing`, `Success`, `Fail`.
- On success it returns a `file_id`; fetch the file with `GET /v1/files/retrieve?file_id=` → `download_url`.
- **That URL lasts 1 h per the video-download page, but 9 h per the async-TTS page.** Assume 1 h. ([query](https://platform.minimax.io/docs/api-reference/video-generation-query.md), [download](https://platform.minimax.io/docs/api-reference/video-generation-download.md))

**Video v2 (MiniMax-H3)**
- `POST /v2/video_generation` → `GET /v2/query/video_generation/{id}`.
- Returns `content.url`; re-query to refresh an expired URL. Tasks are queryable for 7 d.
- `usage` reports seconds ([v2 query](https://platform.minimax.io/docs/api-reference/video-generation-v2-query.md)).

**Callbacks**
- MiniMax first sends a challenge that must be echoed within 3 s, then POSTs status changes.

**Speech**
- `POST /v1/t2a_v2` (sync; hex or URL output, URL valid 24 h). Returns `extra_info.usage_characters`. A US-West host `api-uw.minimax.io` is documented for this endpoint.
- Async: `/v1/t2a_async_v2` → `/v1/query/t2a_async_query_v2?task_id=` ([t2a](https://platform.minimax.io/docs/api-reference/speech-t2a-http.md)).

**Music**
- `POST /v1/music_generation`.
- **Paid APIs closed to new users from 2026-08-20** ([music](https://platform.minimax.io/docs/api-reference/music-generation.md)).

**Image**
- `POST /v1/image_generation` (sync, URL valid 24 h).

**Idempotency, list, cancel, balance:** none documented.

**Pricing** ([paygo](https://platform.minimax.io/docs/guides/pricing-paygo.md))
- Hailuo-2.3 768P 6 s: $0.28.
- H3: $0.08/s.
- speech-2.8-hd: $100 per 1M characters.
- image-01: $0.0035.

**Rate limits** ([limits](https://platform.minimax.io/docs/guides/rate-limits.md))
- Hailuo 20 RPM; H3 300 RPM / 30 in-flight.

**US availability**
- Global host; no country list found (UNVERIFIED).

### BytePlus ModelArk (Seedream, Seedance)

**Host**
- `https://ark.ap-southeast.bytepluses.com/api/v3`.

**US availability**
- **The US is not in the supported-country list** ([availability](https://docs.byteplus.com/en/docs/ModelArk/availability)).

**Seedance** ([create](https://docs.byteplus.com/en/docs/ModelArk/create-video-generation-task-api))
- Submit: `POST /contents/generations/tasks` with `callback_url`, `execution_expires_after`, and `service_tier: flex` (50% off; not for 2.x).
- Get: `GET …/tasks/{id}`. Statuses: `queued`, `running`, `succeeded`, `failed`, `cancelled`, `expired`.
- List: `GET …/tasks?filter.*` (7 d).
- `DELETE …/{id}` cancels a queued task or deletes a finished one.
- Callbacks retry 3 times.

**Seedream**
- `POST /images/generations` (sync, `url` or `b64_json`).

**Output**
- URLs valid 24 h; Seedance 2.5 allows at most 100 downloads per URL.

**Cost** ([pricing](https://docs.byteplus.com/en/docs/ModelArk/1544106))
- Video: `usage.completion_tokens`, priced per 1M tokens. Seedance 2.0 is $7.0, 2.5 is $10.70.
- Image: per image. Seedream 4.0 $0.03, 4.5 $0.04, 5.0-pro $0.045+.

**Balance:** UNVERIFIED.

**Rate limits**
- Individual accounts: 180 RPM / 3 concurrent.

### Tencent HY 3D Global (Hunyuan3D)

**API style** ([API index](https://www.tencentcloud.com/document/api/1284/75531))
- Tencent Cloud API 3.0 at `hunyuan.intl.tencentcloudapi.com`, signed with TC3-HMAC-SHA256.
- `X-TC-Version: 2023-09-01`.
- Regions: `ap-singapore`, `eu-frankfurt`, **`na-siliconvalley`**.

**Actions**
- Pro: `SubmitHunyuanTo3DProJob` / `QueryHunyuanTo3DProJob`.
- Rapid: `SubmitHunyuanTo3DRapidJob` / `QueryHunyuanTo3DRapidJob`.
- Statuses: `WAIT`, `RUN`, `FAIL`, `DONE`.
- Returns `ResultFile3Ds[].Url` (GLB, OBJ, FBX, STL, USDZ…), **valid 24 h**. The JobId is also valid 24 h.
- No callbacks, list, or cancel.

**Cost** ([billing](https://www.tencentcloud.com/document/product/1284/75281))
- Credits per job: Pro Normal 25, Rapid 15, +10 for PBR.
- Postpaid: $0.02 per credit.
- Not reported in the response.

**Balance**
- Cash only, via Billing `DescribeAccountBalance`. No credit-pack API (UNVERIFIED).

**Rate limits**
- 20 rps; concurrency 3 (Pro) / 1 (Rapid).

**US eligibility**
- A US region exists, but an explicit US-eligibility statement is UNVERIFIED. Activation needs real-name verification.

### ElevenLabs (`api.elevenlabs.io`, `xi-api-key`)

**All sync**
- TTS: `POST /v1/text-to-speech/{voice_id}`, with `/stream` and `/with-timestamps` variants.
- Models: `eleven_v4`, `eleven_v4_turbo`, `eleven_v3`, `multilingual_v2`, `flash_v2_5` ([TTS](https://elevenlabs.io/docs/api-reference/text-to-speech/convert), [models](https://elevenlabs.io/docs/overview/models)).
- Sound effects: `POST /v1/sound-generation` with `duration_seconds` 0.5–30 and `loop` ([sfx](https://elevenlabs.io/docs/api-reference/text-to-sound-effects/convert)).
- Music:
  - `POST /v1/music`: 3–600 s; models `music_v1`, `music_v2`, `music_v2_5`.
  - `/v1/music/plan` is free.
  - `/v1/music/detailed` and `/stream` variants ([music](https://elevenlabs.io/docs/api-reference/music/compose)).

**Recovery** ([history](https://elevenlabs.io/docs/api-reference/history/list))
- TTS: `GET /v1/history` (filter by date and source), then `GET /v1/history/{id}/audio`.
- **SFX and music cannot be retrieved later.**

**Cost**
- The **`character-cost`** response header, plus `request-id` ([intro](https://elevenlabs.io/docs/api-reference/introduction)).
- API prices ([pricing](https://elevenlabs.io/pricing/api)):
  - TTS v4: $0.08 per 1k characters.
  - Flash: $0.04 per 1k characters.
  - SFX: $0.12/min.
  - Music: $0.15/min.

**Quota**
- `GET /v1/user/subscription` returns `character_count`, `character_limit`, `next_character_count_reset_unix` ([sub](https://elevenlabs.io/docs/api-reference/user/subscription/get)).
- Usage analytics: `/v1/workspace/analytics/query/usage-by-product-over-time`. The older `/v1/usage/character-stats` is deprecated.
- The pay-as-you-go USD balance has no API.

**Limits**
- Concurrency by plan, e.g. Creator: 5 multilingual, 10 Flash, 2 music.
- 429 codes: `rate_limit_exceeded`, `concurrent_limit_exceeded`, `system_busy`.

### Tripo (docs at `developers.tripo3d.com`)

**v3 API** ([migration](https://developers.tripo3d.com/en/docs/migration-v2-to-v3.md))
- Base `https://openapi.tripo3d.com/v3`, with one endpoint per task type, e.g. `/generation/image-to-model`, `/models/convert`, `/animations/rig`.
- v2 (`api.tripo3d.com/v2/openapi`) still works alongside it.
- An official Rust SDK exists: `tripo3d-sdk`, from GitHub VAST-AI-Research/tripo-rust-sdk ([SDK](https://developers.tripo3d.com/en/docs/sdk.md)).

**Polling** ([task-query](https://developers.tripo3d.com/en/docs/task-query.md))
- `GET /v3/tasks/{id}` returns `status`, `progress`, `output`, `credits_consumed`.
- Batch: `POST /v3/tasks/list` for up to 100 IDs.
- Statuses: `queued`, `running`, `success`, `failed`, `banned`, `expired`, `cancelled`.

**Webhooks** ([webhooks](https://developers.tripo3d.com/en/docs/webhooks.md))
- Configured in the dashboard. Signed `t=…,v1=` HMAC.
- Dedupe on the `Tripo-Webhook-Delivery` header.

**Not in v3 docs:** a cancel endpoint, and the v2-era WebSocket watch (UNVERIFIED).

**Output**
- `output.model_url`, and **URLs expire after 5 minutes**. Re-query the task for a fresh link ([quick-start](https://developers.tripo3d.com/en/docs/quick-start)).

**Cost**
- `credits_consumed`, a decimal; 1 credit = $0.01.
- Credits are frozen on create and charged only on success.
- Example prices: Image→3D 20–30 credits, auto-rig 25 ([pricing](https://developers.tripo3d.com/en/pricing)).

**Account** ([account](https://developers.tripo3d.com/en/docs/account.md))
- Balance: `GET /v3/account/balance` → `{balance, frozen}`.
- Recovery: `GET /v3/account/usage`.

**Rate limits** ([rate limits](https://developers.tripo3d.com/en/docs/rate-limits.md))
- `X-RateLimit-*` headers.
- Concurrency per category; 429 with code 2000 and `Retry-After`.

### Meshy (`api.meshy.ai`)

**Endpoints**
- Each task type has five routes: `POST`, `GET /{id}`, `GET` (list), `GET /{id}/stream` (SSE), `DELETE /{id}`.
- Paths: `/openapi/v2/text-to-3d`, `/openapi/v1/image-to-3d`, `multi-image-to-3d`, `remesh`, `retexture`, `rigging`, `animations`, `text-to-image` ([image-to-3d](https://docs.meshy.ai/api/image-to-3d.md)).
- Create returns 202.
- Statuses: `PENDING`, `IN_PROGRESS`, `SUCCEEDED`, `FAILED`.

**Cancel**
- `DELETE` on a PENDING task deletes and refunds it.
- On an IN_PROGRESS task it returns 409.

**Webhooks**
- Configured in the dashboard, account-wide, and send every status update. No signature is documented ([webhooks](https://docs.meshy.ai/api/webhooks.md)).

**Idempotency**
- None. After a timeout, check the list endpoint before resubmitting ([errors](https://docs.meshy.ai/api/errors.md)).

**Output** ([retention](https://docs.meshy.ai/api/asset-retention.md))
- Signed `model_urls` (glb/fbx/obj/usdz…), valid until `expires_at`.
- Assets kept up to **3 days** on non-Enterprise plans.

**Cost**
- `consumed_credits` on each task ([changelog](https://docs.meshy.ai/api/changelog.md)).
- Example prices: meshy-6/7.1 textured 30 credits, rigging 5 ([pricing](https://docs.meshy.ai/api/pricing.md)).
- USD per credit is UNVERIFIED.

**Balance**
- `GET /openapi/v1/balance` ([balance](https://docs.meshy.ai/api/balance.md)).

**Rate limits** ([limits](https://docs.meshy.ai/api/rate-limits.md))
- 20 rps; queue of 10 to 100 tasks depending on tier.

**Quirks**
- The shared test key was retired 2026-09-16.
- `meshy-5` was retired 2026-10-10.

### Rodin / Hyper3D (docs at `docs.hyper3d.ai`)

**API**
- Base `https://api.hyper3d.com/api/v2`.
- Submit: `POST /rodin` (multipart) with `tier` (Gen-2.5-* tiers; if omitted, defaults to Regular/Gen-1). Returns `uuid`, `jobs.subscription_key`, `consumed` ([Gen-2.5](https://docs.hyper3d.ai/en/api-specification/rodin-gen2-5)).
- Status: `POST /status {subscription_key}` → `Waiting`, `Generating`, `Done`, `Failed`.
- Download: `POST /download {task_uuid}` → `list[{name,url}]`.
- No webhook or cancel.

**Errors** ([errors](https://docs.hyper3d.ai/en/api-specification/errors))
- **Rejected submissions return HTTP 201 with an `error` field in the body.**

**Recovery**
- `GET /check_balance?usage_page=` lists `task_uuid`s.
- No way to look up a `subscription_key` from a `uuid` is documented.

**Output and retention**
- URLs "can expire" (no TTL given).
- Data kept 7 d ([retention](https://docs.hyper3d.ai/en/legal/data-retention-policy)).

**Plan and cost**
- API access requires the Business plan, $120/mo ([pricing](https://hyper3d.ai/pricing?lang=en)).
- Gen-2.5 base cost: 0.5 credits.
- **Alternative:** Rodin on fal at $0.40 per generation ([fal Rodin](https://fal.ai/models/fal-ai/hyper3d/rodin/v2.5)). This avoids the plan and reuses the fal adapter.

---

## 3. Recommended generic async-job adapter (Rust)

### 3.1 Core trait

```rust
#[async_trait]
trait MediaProvider {
    fn caps(&self, model: &ModelId) -> Caps;        // sync|async, cancel, webhook, client_id, list_recover, url_ttl
    async fn submit(&self, job: &JobSpec, client_id: &str) -> Result<SubmitOutcome>; // Remote{remote_id, aux}, or Done(Output) for sync
    async fn status(&self, h: &RemoteHandle) -> Result<JobState>;   // normalized
    async fn cancel(&self, h: &RemoteHandle) -> Result<CancelOutcome>; // Unsupported | Requested | NotCancellable
    async fn result(&self, h: &RemoteHandle) -> Result<Vec<Artifact>>; // Url{url, auth_header?, expires_at?} | Inline(bytes)
    async fn cost(&self, h: &RemoteHandle, resp: &Raw) -> Result<CostRecord>; // Exact | Units | Estimated
    async fn recover(&self, intent: &JobIntent) -> Result<Option<RemoteHandle>>; // reconcile orphaned submits
    async fn balance(&self) -> Result<Option<Balance>>;
}
```

`RemoteHandle` carries every ID a provider needs. For example, Rodin needs `uuid` + `subscription_key`, MiniMax v1 needs `task_id` + `file_id`, and Veo needs the operation `name`.

Normalized `JobState`:
- `Queued{pos}`
- `Running{progress}`
- `Succeeded`
- `Failed{code, billable}`
- `Cancelled`
- `Expired`
- `Unknown`

Status mapping is provider-specific. For example:
- fal: `COMPLETED` with `error` set maps to `Failed`.
- Rodin: HTTP 201 with an `error` body is a submit failure.
- NanoGPT: statuses need case-insensitive matching.

### 3.2 Job state machine, persisted in SQLite (WAL)

`Draft → Submitting → Submitted(remote_id) → Running → Terminal → Downloading → Stored → Costed`

1. **Write-ahead intent.** Insert the row with a local ULID `client_id`, a `request_hash` (provider + model + canonical input JSON + seed), and `state=Submitting` **before** the HTTP call. Commit it.
2. **Submit.**
   - Send `client_id` wherever the provider accepts one: Kling `external_task_id`; fal/Replicate webhook query string; Gemini `labels`; Batch `custom_id` / `key`.
   - Persist the remote ID in the same transaction as the state change.
   - Read cost-bearing headers and fields (`X-Fal-Billable-Units`, `character-cost`, `X-Request-ID`, `X-Generation-Id`) immediately.
3. **Never auto-resubmit a row stuck in `Submitting`.** On startup, run `recover()`:

   | Provider | How to recover an orphaned submit |
   |---|---|
   | Kling | `GET /tasks?external_task_ids=` (exact match) |
   | fal | `requests/by-endpoint?expand=payloads`, match on `json_input` |
   | Replicate | `GET /v1/predictions?created_after=`, match on input (≤1 h) |
   | NanoGPT video | `/generate-video/recover` |
   | Alibaba | `GET /api/v1/tasks` (time window + model, ≤24 h) |
   | BytePlus | list tasks (≤7 d) |
   | Tripo | `/v3/account/usage` |
   | Rodin | `check_balance?usage_page` |
   | Meshy | list by `-created_at` |
   | ElevenLabs TTS | `/v1/history` |
   | Veo | `models/{m}/operations` |

   If there is no match, or the provider has no lookup (OpenRouter video, MiniMax, Tencent, Gemini Interactions, OpenAI sync, ElevenLabs SFX/music), mark the row `Orphaned` and **ask the user** before resubmitting. Show the possible double charge.
4. **Dedupe at the app layer.** A UI-level unique index on `(request_hash, state ∈ in-flight)` prevents accidental double clicks. Explicit "regenerate" gets a new seed or hash.

### 3.3 Status: polling first, webhooks optional

This is a desktop app. Most webhook senders drop or can't reach private IPs: fal drops private targets, and Google, Kling and BytePlus need a public URL. **Make polling the baseline:**

- A scheduler with per-provider intervals:
  - Alibaba ~15 s.
  - Rodin 5→30 s.
  - Tripo 1–2 s; batch the polls with `/v3/tasks/list`.
  - Meshy: `Retry-After`, or 5 s.
  - Everything else: exponential backoff from 2 s to 30 s, with jitter.
- Use streaming where it exists: fal `/status/stream`, Meshy `/stream` SSE, Gemini interaction streams. Fall back to polling when a stream drops.
- Optional webhook relay (e.g. a tiny cloud function) feeding the same `on_status` path.
  - Verify signatures: fal Ed25519/JWKS; Replicate, OpenAI, Gemini static and Kling all use Standard-Webhooks-style HMAC; OpenRouter, Tripo, BytePlus as documented.
  - Dedupe on the delivery-ID header.
  - Treat webhooks only as a hint to poll now.

### 3.4 Download policy (URL TTL is the main data-loss risk)

- On `Succeeded`, download **immediately**, before marking the job done. Write to a temp file, fsync, hash, then move into the content store. Store `sha256`, MIME type and size.
- Per-provider `url_ttl` drives priority and retry:
  - **Tripo: 5 min.** Re-query the task for a fresh URL.
  - **MiniMax File Retrieve: 1 h.** Re-retrieve if needed.
  - **Replicate: 1 h.** Hard limit; no refresh.
  - NanoGPT images: ~1 h.
  - **Gemini Veo: 2 d.**
  - Alibaba, BytePlus, Tencent, MiniMax image/TTS: 24 h.
  - Meshy: `expires_at` (≤3 d).
  - Kling: 30 d.
  - fal: per the lifecycle header (set it explicitly).
  - OpenRouter: `expires_at`.
- Some artifacts need authentication to download:
  - OpenRouter `/content`: Bearer.
  - Gemini Veo URI: `x-goog-api-key`, and follow redirects.
- Strip auth headers on cross-origin redirects.
- Inline outputs never expire but must be persisted before ack. Applies to OpenAI/Gemini base64 and ElevenLabs bytes. **ElevenLabs SFX and music can't be re-fetched.**
- For Gemini Omni with URI delivery, capture the URI from the **create** response; a later GET returns base64 instead.
- Optionally ask the provider to delete data after download: fal `DELETE …/payloads`, BytePlus `DELETE` task, Gemini `DELETE /interactions/{id}`.

### 3.5 Cost model

Store a `CostRecord {provider, model, units: [(unit, qty)], usd_estimate, usd_exact?, currency, source: Exact|Usage×Price|PriceTable, price_table_version}`.

| Source | Providers |
|---|---|
| **Exact USD in response** | OpenRouter (`usage.cost`), NanoGPT (`cost` + currency), Kling (`billing[].amount`) |
| **Exact via follow-up** | fal (`billing-events?request_id=`, BILLING key; batch 50 IDs), OpenRouter `/generation?id=`, NanoGPT `/usage/requests/{id}` (24 h) |
| **Provider credits in response** | Tripo `credits_consumed` (×$0.01), Meshy `consumed_credits`, Rodin `consumed`, ElevenLabs `character-cost` header |
| **Units in response × local price table** | OpenAI tokens; Gemini tokens / seconds; Alibaba seconds / images; BytePlus tokens / images; MiniMax v2 seconds / characters; Replicate `predict_time` × hardware rate |
| **Price table only** | Tencent credits, MiniMax v1 video, Gemini Veo (per second, success only), OpenAI TTS (characters) |

Keep the price table as versioned JSON with `effective_from` dates, because prices change (e.g. Gemini TTS doubles 2027-01-01). Sync it from fal `/v1/models/pricing`, OpenRouter models `pricing_skus`, and NanoGPT model endpoints.

Record a `pre_estimate` before submit (fal `/pricing/estimate` where available) to drive the UI's budget guard. Then reconcile to the exact or post-hoc value.

Model the billing rules that differ by provider:
- Charged on create, refunded on failure: Meshy, NanoGPT video, Tripo (freeze model).
- Charged only on success: Gemini Veo, Alibaba, BytePlus, Tencent, Replicate failures.
- A fal 422 can still be billed.

### 3.6 Balance and limits

`balance()` support:

| Support | Providers |
|---|---|
| Has an API | fal, OpenRouter `/key`, NanoGPT, Kling, Tripo, Meshy, Rodin, ElevenLabs quota, Alibaba BSS / Tencent Billing (separate signing) |
| No API: return `None`, rely on the local ledger, treat 402 as "out of funds" | Replicate, OpenAI (admin-key costs only), Gemini, MiniMax, BytePlus |

Use a concurrency limiter per provider, and per model where it matters:

| Provider | Default slots |
|---|---|
| fal | 2–40 (by spend tier) |
| ElevenLabs | per plan |
| Tencent | 3 (Pro) / 1 (Rapid) |
| Alibaba Wan | 5 |
| Tripo | per category |
| Meshy | queue size |
| BytePlus | 3 |
| Kling | per resource pack |

- Retry 429/5xx with backoff, honouring `Retry-After`.
- **Never retry body-level errors.** Rodin returns 201 with an error, and Tripo returns 403 with code 2010 for insufficient credits.
- Treat OpenRouter `402 in_flight_budget_exhausted` as retryable.

### 3.7 Per-service quirks checklist

- **fal**
  - Cancel is `PUT`; failure arrives as `COMPLETED` + `error`.
  - Disable fallback (`x-app-fal-disable-fallback`) if price determinism matters.
  - Use two keys: API + BILLING.
  - Set the object-lifecycle header.
- **Replicate**
  - Everything is gone after 1 h.
  - Always send `Cancel-After`.
  - Use `/models/{o}/{n}/predictions` for official models.
- **OpenRouter**
  - Two image paths with different response shapes.
  - Video has no cancel or list.
  - TTS cost comes only from `X-Generation-Id` → `/generation`.
- **NanoGPT**
  - Mixed `/api` vs `/api/v1` paths.
  - Status casing differs between docs and schema.
  - Echo `cost` on TTS status polls to get refunds.
  - Balances are strings; currency is USD or XNO.
- **OpenAI**
  - No video.
  - Image model churn: pin the snapshot.
  - Images are sync and can take ~2 min (set long timeouts), or use Responses background mode.
  - No balance API.
- **Gemini**
  - Prefer Interactions; `background:true` for long jobs.
  - Veo is not cancellable and is kept 2 d.
  - Omni URI only on create.
  - No cost or balance API.
  - Batch is non-idempotent and expires after 48 h.
- **Alibaba**
  - Region-bound keys and workspace host.
  - Async header required for video.
  - 24 h list window.
- **Kling**
  - Always send `external_task_id`.
  - Choose between new and legacy APIs per model.
  - JWT expiry is 30 min if using AK/SK.
- **MiniMax**
  - v1 needs a second File Retrieve hop with a short-lived URL.
  - Callback challenge handshake.
  - Music closed to new users.
- **BytePlus**
  - Likely not available in the US; gate it behind a region check.
  - Seedance URLs limited to 100 downloads.
- **Tencent**
  - TC3 signing.
  - No list: a crash before persisting `JobId` means the job is lost.
  - Rapid concurrency is 1.
- **ElevenLabs**
  - Sync only: record `character-cost` and `request-id` headers.
  - Persist SFX and music bytes before ack.
- **Tripo**
  - 5-minute URLs.
  - `credits_consumed` is decimal.
  - Official Rust SDK available.
- **Meshy**
  - DELETE cancels only PENDING tasks.
  - 3-day retention.
  - Queue-full 429 has no `Retry-After`.
- **Rodin**
  - Store `uuid` + `subscription_key`.
  - Check the body `error` on 201.
  - Requires the Business plan; consider fal-hosted Rodin instead.

### 3.8 Local ComfyUI

The same trait applies to local ComfyUI:
- `submit` = `POST /prompt` with a `client_id`.
- `status` = `/history/{prompt_id}` plus the websocket.
- `cancel` = `/interrupt` or a queue delete.
- `result` = `/view`.
- `cost` = local GPU-seconds only, at $0.

Outputs are local files, so there is no TTL.
