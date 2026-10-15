# Claude Code integration facts (verified 2026-10-12 against code.claude.com docs; changelog read 2.1.288–2.1.296)

## MCP
- Two client runtimes (`MCP_SDK_GENERATION=v1|v2`); stdio servers request protocol 2026-07-28 by default since 2.1.292 (`MCP_PROTOCOL_NEGOTIATION=legacy` opts out).
- Supported: tools (+ `list_changed`), resources (`@server:protocol://path`, list/read tools), prompts as commands (`/mcp__<server>__<prompt>`), elicitation form + URL.
- **Not documented / unconfirmed:** `structuredContent`, `outputSchema`, `resource_link`, resource subscriptions, input-required (MRTR), tool annotations affecting permissions. → Return plain JSON text + file paths; treat others as optional extras.
- `_meta["anthropic/requiresUserInteraction"]: true` (v2.1.214+): always prompts in every mode; allow rules can't skip it; denied in dontAsk; `--permission-prompt-tool` converts allow → deny for it.
- `_meta["anthropic/alwaysLoad"]` (2.1.285+), `_meta["anthropic/maxResultSizeChars"]` (≤500,000).
- Description limit: docs say 2,048 (changelog 2.1.296: 4,096 up-front); override `CLAUDE_CODE_MAX_MCP_DESCRIPTION_LENGTH`. **Design to ≤2,000.** Tool names >128 chars dropped.
- Tool search on by default (`ENABLE_TOOL_SEARCH`); server `alwaysLoad` exempts; write good server `instructions`.
- Timeouts: `MCP_TOOL_TIMEOUT` (~28 h default), per-server `"timeout"` ms; calls running >2 min move to background (`CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS`); stdio idle timeout 30 min; progress doesn't extend timeouts. `MAX_MCP_OUTPUT_TOKENS` 25,000; >50,000 chars saved to file. → long work = job id + poll tools.
- Plugin bundling: `.mcp.json` at plugin root or `mcpServers` in plugin.json; `${CLAUDE_PLUGIN_ROOT}`, `${CLAUDE_PLUGIN_DATA}`, `${CLAUDE_PROJECT_DIR}` substituted (forward slashes on Windows). Tool names `mcp__plugin_<plugin>_<server>__<tool>`. Plugin subagents can't carry mcpServers/hooks/permissionMode.
- `userConfig` in plugin.json (types string/number/boolean/directory/file; `sensitive:true` → secure store); `${user_config.KEY}` in MCP config; `CLAUDE_PLUGIN_OPTION_<KEY>` in hooks.

## Hooks (plugin `hooks/hooks.json` supported)
- Events incl. SessionStart (additionalContext, initialUserMessage, sessionTitle, watchPaths), UserPromptSubmit, PreToolUse (`permissionDecision: allow|deny|ask|defer`, `updatedInput`, `additionalContext`; deny>defer>ask>allow; hooks don't bypass deny/ask rules), PostToolUse (`updatedToolOutput`, `updatedMCPToolOutput`, `mcp_server`), Notification (matchers incl. permission_prompt, idle_prompt, elicitation_*; `terminalSequence` for desktop notifications), Elicitation/ElicitationResult (can auto-answer), PermissionRequest, Stop, SubagentStop, FileChanged, CwdChanged, SessionEnd, Pre/PostCompact…
- Matchers: exact/list or regex; MCP `mcp__<server>__.*`; Bash filtered by `"if": "Bash(git *)"` permission-rule syntax.
- Handler types: command, http, mcp_tool, prompt, agent; `onFailure: "block"` (2.1.295+).
- `defer` only in `-p`: process exits with `deferred_tool_use`, caller resumes with `--resume` + allow/updatedInput.

## Launching / handoff
- CLI: `claude "prompt"`, `-p`, `-c`, `-r <id|name>`, `--session-id`, `-n`, `--fork-session`, `--add-dir`, `--permission-mode`, `--append-system-prompt(-file)`, `--mcp-config`, `--strict-mcp-config`, `--plugin-dir`, `--settings`, `--model`, `--effort`; cwd = child process cwd. Background sessions: `claude --bg "prompt"`, `claude agents --json`, `claude attach`, `logs`, `stop`.
- **Deep link (terminal):** `claude-cli://open?q=<prompt>&cwd=<abs path>&repo=<owner/name>` — new terminal window, prompt pre-filled, not sent; q ≤5,000 chars; cwd absolute local, no `..`; handler registered (HKCU\Software\Classes\claude-cli) only after the first interactive prompt; `disableDeepLinkRegistration` opt-out.
- Desktop: `claude://code/new` documented for SSH only; `claude --desktop [--continue|--resume <id>]` opens Desktop on cwd (no prompt; v2.1.285+, x64 Windows, subscription). No documented local API to start a Desktop session with a prompt.
- VS Code: `vscode://anthropic.claude-code/open` (no documented params).

## Headless
- `claude -p … --output-format stream-json --verbose --include-partial-messages`; `--input-format stream-json` for multi-turn; `--output-format json` has `total_cost_usd`, `session_id`, `structured_output` (`--json-schema`); `system/init` lists plugins/MCP errors; `--max-turns`, `--max-budget-usd`, `--include-hook-events`, `--no-session-persistence`; `--permission-prompts none`; **`--permission-prompt-tool <mcp tool>`** routes approvals to an MCP tool; SIGINT ends turn cleanly; background work awaited ≤10 min.
- `--bare` ignores OAuth (needs API key).

## Subscription terms (quoted from legal-and-compliance / agent-sdk overview)
- "OAuth authentication is intended exclusively for purchasers of Claude Free, Pro, Max, Team, and Enterprise subscription plans and is designed to support ordinary use of Claude Code and other native Anthropic applications."
- "Developers building products or services … including those using the Agent SDK, should use API key authentication … Anthropic does not permit third-party developers to offer Claude.ai login into their own applications, or to route requests through Free, Pro, or Max plan credentials on behalf of their users. Moreover, developers may not collect, store, or intermediate Claude.ai credentials or session tokens."
- "Nor does it prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription…"
- Reading (not verbatim authorization): the suite may drive the user's own, unmodified, locally installed `claude` binary signed in with the user's own subscription, for that user's own use — as long as the suite never reads/stores/proxies OAuth tokens or `setup-token` output and never patches the binary. Each user of the suite runs their own binary with their own sign-in; the suite never routes requests through anyone else's credentials. Agent SDK with subscription login: not recommended (docs direct SDK developers to API keys).

## Usage / telemetry
- OTel: `CLAUDE_CODE_ENABLE_TELEMETRY=1`, `OTEL_METRICS_EXPORTER=otlp`, `OTEL_LOGS_EXPORTER=otlp`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL` (grpc|http/json|http/protobuf). Metrics `claude_code.token.usage` (type input/output/cacheRead/cacheCreation, model, plugin/skill/agent/MCP attribution), `claude_code.cost.usage` (USD estimate), session/lines/commit counts, active time; events incl. api_request, tool_result, tool_decision. **Not configurable per project** (ignored in `.claude/settings*.json`): set in `~/.claude/settings.json` `env`, managed settings or shell.
- Subscription quota: statusline JSON `rate_limits.five_hour.used_percentage`, `rate_limits.seven_day.used_percentage` (+ `resets_at`), Pro/Max only, interactive only, after the first response. `/usage` interactive. No programmatic quota API.
- Statusline input also has `cost.total_cost_usd`, `context_window.*`, `model`, `session_id`, `workspace.*`.

## Permissions
- Rules: `mcp__<server>__*`; plugin servers `mcp__plugin_<plugin>_<server>__*`; deny → ask → allow, first match; allow can't override ask/deny; hook allow doesn't bypass rules.
- Pattern: broad allow for the suite's MCP tools; destructive/exec tools carry `requiresUserInteraction` (server-owned, can't be overridden) plus optional `ask` rules.

## Channels (research preview)
- MCP server declaring `capabilities.experimental['claude/channel']` pushes `notifications/claude/channel` into a running interactive session; optional permission relay (`claude/channel/permission`). Needs `--channels` / `--dangerously-load-development-channels server:<name>` (confirmation prompt), not in `-p`/SDK; a channel server negotiating 2026-07-28 on the v2 client isn't registered (use legacy negotiation).
