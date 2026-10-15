# Chinese LLM providers, part 1 (Tencent, Xiaomi MiMo, StepFun) — verified 2026-10-12, primary sources

| Provider | Intl base URL | Styles | Cost in response | Balance/usage API | Currency |
|---|---|---|---|---|---|
| Tencent (TokenHub; old Hunyuan platform frozen) | `tokenhub-intl.tencentcloudmaas.com` (SG), **`tokenhub-us.tencentcloudmaas.com` (Silicon Valley)** | `/v1/chat/completions`, `/v1/responses`, `/v1/messages` (Anthropic, `x-api-key`) | No | Usage `DescribeUsageRankList` via TC3-HMAC-signed control plane (SecretId/Key, unverified); no balance endpoint | USD |
| Xiaomi MiMo | `api.xiaomimimo.com/v1`, `/anthropic` (pay-as-you-go `sk-` keys; Token Plan `tp-` keys on separate hosts) | chat, Responses (limited), Anthropic Messages | No | None (console only) | USD overseas, CNY domestic |
| StepFun | `api.stepfun.ai/v1` (CN: `.com`) | chat, Responses (2 models), `/v1/messages` (Bearer) | No | **`GET /v1/accounts`** with the inference key (balance, cash, voucher; no currency field) | USD on .ai, CNY on .com |

## Quirks for the generic adapter
- Reasoning text: `reasoning_content` (Tencent, MiMo) vs `reasoning` (StepFun default; `reasoning_format:"deepseek-style"` switches to `reasoning_content`).
- Cached tokens: StepFun top-level `usage.cached_tokens`; others `prompt_tokens_details.cached_tokens`.
- Thinking toggle: `thinking:{type:enabled|disabled}` (Tencent, MiMo); StepFun Anthropic path uses `output_config.effort`.
- `tool_choice`: MiMo only `auto` (others silently stripped); StepFun Responses only `auto`.
- Structured output: StepFun `json_schema` + `strict` on recent models; MiMo `json_object` only; Tencent unverified.
- Tencent: errors inside SSE frames; 429 sub-codes 429001–429006 with `Retry-After`; billing errors 403004 / 402-401007/8; error `code` int or string; key/base-URL site mismatch → 401002.
- MiMo: thinking forces temperature/top_p; model IDs `mimo-v2.5*` retire 2026-10-21 (UTC+8) → v2.6.
- StepFun: limits tiered by cumulative top-up (V0 5 concurrent/100 RPM/500k TPM … V4).
- None return cost; none document rate-limit quota headers (Tencent `Retry-After` only).

## Prices (USD per 1M tokens, cache-miss input / cache-hit / output)
- Tencent: Hy3 $0.132 / $0.033 / $0.528; Hy4-preview $0.834 / $0.042 / $2.501.
- MiMo: v2.6-pro $0.435 / $0.0036 / $0.87; v2.6-flash $0.14 / $0.0028 / $0.28; batch 50%.
- StepFun: step-5-preview $1.00 / $0.10 / $2.70; step-3.7-flash $0.20 / $0.04 / $1.15; step-3.5-flash $0.10 / $0.02 / $0.30.

Sources: tencentcloud.com docs 1300/78941, 80632, 80695, 78937, 78936, 81012, 82540, 82348; mimo.mi.com docs (api-integration, responses, openai-api, anthropic-api, list-models, structured-output, price, rate-limit); platform.stepfun.ai docs (messages, responses, models, chat completion, json-mode, prompt-cache, pricing, accounts, error codes).
