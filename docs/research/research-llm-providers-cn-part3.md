# Chinese LLM providers, part 3 (Z.ai/GLM, MiniMax, BytePlus ModelArk) — verified 2026-10-12, primary sources

| Provider | Intl base URL | Styles | Cost in response | Balance API | Currency | US availability |
|---|---|---|---|---|---|---|
| Z.ai / GLM | pay-as-you-go `https://api.z.ai/api/paas/v4`; Coding Plan: OpenAI `/api/coding/paas/v4`, Anthropic `/api/anthropic`, Responses `/api/v1` | OpenAI chat (pay-as-you-go); Anthropic + Responses documented for Coding Plan (pay-as-you-go Anthropic billing unverified) | No | None | USD intl / CNY China | Yes (intl platform) |
| MiniMax | `https://api.minimax.io/v1`, Anthropic `…/anthropic` (recommended protocol) | chat, Responses, Anthropic (Bearer or `x-api-key`) | No | None (console / `mmx quota`) | USD intl / CNY China | Yes |
| BytePlus ModelArk | `https://ark.ap-southeast.bytepluses.com/api/v3` | chat, Responses; Anthropic only for Coding Plan | No | None (AK/SK Billing OpenAPI: bill details only) | USD | **United States not in the supported-country list** (docs updated 2026-04-21) — treat as unavailable unless sales confirms |

## Quirks
- **Z.ai:** `tool_choice` only `auto`; `json_object` only (no `json_schema`); tool-call `function.arguments` typed as an **object** in responses; extra `finish_reason` values (`sensitive`, `model_context_window_exceeded`, `network_error`); `thinking:{type, clear_thinking}` + `reasoning_effort` (GLM-5.3 rejects disabled); `reasoning_content`; `stream_options.include_usage` undocumented; no model-list endpoint documented; errors 1113 insufficient balance, 1302/1305/13xx limits; no rate-limit headers.
- **MiniMax:** `n` must be 1; penalties/logit_bias ignored; no `json_schema` for chat; `thinking.type` disabled|adaptive; `reasoning_split:true` → `reasoning_content`, otherwise `<think>` tags inside `content`; send full assistant message back for tool loops; error envelope `base_resp {status_code, status_msg}` (1002 rate, 1008 balance, 1004 auth); priority tier 1.5×; price doubles above 512k input; caching ≥512 tokens (automatic free writes; explicit `cache_control` 5-min TTL).
- **BytePlus:** most OpenAI-complete (json_schema+strict beta, tool strict, parallel_tool_calls, tool_choice incl. required); `chunk_include_usage`; `thinking` via extra_body, `reasoning_effort`; tiered by prompt length; **cache storage billed per hour**; off-peak window on some models (UTC+8); flex ~50%; batch ~50%; `X-Client-Request-Id` header for idempotency/tracing.

## Prices (USD per 1M tokens)
- Z.ai GLM-5.3: $1.4 in / $0.26 cached / $4.4 out.
- MiniMax-M3 (≤512k): $0.30 in / $0.06 cache read / $1.20 out.
- BytePlus seed-2-0-lite (≤128K): $0.25 / $0.05 / $2.00; cache storage $0.0083 per M tokens per hour.

Sources: docs.z.ai (api-reference/introduction, chat-completion, devpack quick-start, glm-5.3, thinking, struct-output, pricing, cache, api-code); docs.bigmodel.cn; platform.minimax.io docs (text-chat-openai, responses-create, text-chat-anthropic, api-overview, list-models, pricing-paygo, prompt caching, rate-limits, m-plan usage rules); docs.byteplus.com ModelArk (availability, 1298459, 1330626, 1585128, 1928262, 1494384, 1544106, 1346559, 1848593, ListBillDetail); volcengine docs 82379/1928261.
