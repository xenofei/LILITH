# LILITH glossary

The one home for the suite's settled terms. Each row gives a term one meaning and points to the rules that define the behaviour; it never restates the behaviour. Rule ids are the spec's (`docs/superpowers/specs/2026-10-12-lilith-design.md`).

- **Avoid** lists names that new text must not use for the term, usually because they mean something else here. When the spec uses a word in two senses, each sense has its own row and a qualified name. Older spec text still uses some Avoid names (`run`, `store`, `profile`, `lease` alone); fix them when you edit that rule for another reason, not as a change of their own.
- **Same change (QA-089):** a new term enters this file in the same change that introduces it, and a renamed term keeps its old name under Avoid.
- Terms only, no decisions or implementation detail: decisions are `(decision)` rules in the spec, with context in `docs/decisions/`.

## Suite, apps and processes

| Term | Meaning | Avoid | Defined in |
|---|---|---|---|
| Core (`lil-core`) | The per-user headless Rust service that owns all workspace state and engines; the only writer of workspace files. | server, daemon, backend | ARCH-001, ARCH-002, ARCH-004 |
| App | A Qt/QML process that is a client of the core and is usable on its own (for example `lil-image`). | module (a core part), plugin | APP-001, ARCH-003 |
| Studio | An app that is an asset generator: you describe an asset, get takes and revise them. Each has a proper name: ATHANOR, the Image studio; ALEMBIC, the 2D studio; CRUCIBLE, the 3D studio; AETHER, the Audio studio; MERCURIUS, the Animation studio; TERRA, the World studio; GRIMOIRE, the Narrative studio; SPECULUM, the Video studio; HOMUNCULUS, the Avatar studio. Window titles and splashes use the proper name; launchers and menus lead with the plain name. | Prompt Studio (a tab, not a studio), module (for a studio), app (when the asset loop is meant) | APP-161, APP-164, IMG-001 |
| Image studio (ATHANOR) | The first studio, `lil-image`: canvas, ComfyUI graph and embedded ComfyUI web UI. | image app, image editor | IMG-001, IMG-002, APP-164 |
| LILITH (the shell) | The shared shell every studio runs in; it has no other name, and the Asset browser and Review are panels in it. | Hub (the later app), studio | APP-164 |
| Prompt Studio | In M1, a main-area tab of the Image studio, backed by the core module `prompt`, that turns a request into a brief and compiles it; a standalone app is Later. | studio (it is not one) | APP-149, AI-157 |
| Hub | The later `lil-hub` app that takes the shared panels over from the Image studio once a second app exists. Also the H in the suite's name, which is not a component. | Home panel, core | ARCH-011, ARCH-010 |
| Shared panels | The suite-wide panels (Home, Asset browser, Inspector, Review, Libraries, Jobs, Confirmations, Usage & spend and others) that the Image studio hosts in M1. | studio panels | ARCH-010, APP-008 |
| Module (core) | One part of `lil-core` with one job, such as `store`, `jobs` or `meter`. | app, service | CORE-014 |
| `lil` | The suite's CLI: debugging, hooks, setup checks, scheduled tasks and the test harness; caller kind `cli`. | | ARCH-016 |
| `lil-mcp` | The stdio MCP server Claude Code spawns; protocol adaptation only, no policy or state. | MCP plugin | ARCH-012, ARCH-014 |
| Plugin 1.3 | The suite's Claude Code plugin (MCP server plus hooks), shipped with the suite; its id keeps the pre-rename name. | marketplace, extension | ARCH-101, CORE-151 |
| Toast | A Windows notification the core posts under the channel's AUMID; Windows draws it, and a click opens a `lil:` URL in an app. | popup, alert (an alert is a Hardware or cap event) | APP-022, ARCH-005 |
| Notification category | One of the toast kinds, each with its own toggle. | notification type | APP-146 |
| Activation scheme | The URL scheme a channel's toasts and links use: `lil`, `lil-dev` or `lil-test-<id>`. | protocol (the pipe protocol is another thing) | APP-020, Appendix G |
| Deploy folder | The trimmed `windeployqt` folder that runs with Qt and MSVC off PATH. | install folder (Velopack's) | ARCH-114, OPS-094 |

## Workspace, assets and documents

| Term | Meaning | Avoid | Defined in |
|---|---|---|---|
| Workspace | A folder holding projects, assets, registries and logs, versioned with git and LFS; `suite.toml` lists the known ones, and in M1 the core serves one. | project (one level down), repo | Appendix E (workspace format I1), ARCH-090, ARCH-123 |
| Project | A folder of assets inside a workspace, with its own `project.yaml` settings; the active project is per client session. | workspace | ARCH-088, CORE-057 |
| Workspace format | The on-disk schemas for workspace, project, asset manifests, registries and job records. | manifest format (only one part) | CORE-178, CORE-148, Appendix E |
| Asset | A tracked deliverable in a project, with a manifest, a status and sealed versions. | file, item (a library item is different) | CORE-023, CORE-048 |
| Version (sealed version) | An immutable `v<semver>/` folder of an asset, made by sign-off; in copy, "version" means only this. | revision, take, snapshot (a snapshot is an autosave) | CORE-026, ARCH-087, APP-163 |
| Candidate | The spec's and the code's word for one output of a candidate run, waiting for review; user-facing copy calls it a take. | draft, proposal (a proposal belongs to a document) | CORE-023, APP-057 |
| Take | The user-facing name for one candidate: Take 1, Take 2 and so on, grouped by revision round. | version (only a sealed result), candidate (in copy) | APP-163 |
| Candidate run | One generation for an asset, writing its outputs to `candidates/<run_id>/` and going through QA and repair. | job (the work that makes it), run (alone) | CORE-023, CORE-025 |
| Sign-off | Choosing a candidate and sealing it as a new version; ungated for you, gated for agents by failed technical QA. | approve, accept, publish | CORE-026, CORE-051, CORE-132 |
| Seal | The step of sign-off that moves the chosen files into a read-only version folder with a frozen manifest snapshot. | lock, freeze, commit | CORE-026 |
| Human edit | An outside change to a candidate or sealed file, recorded and offered as a new version. | manual edit, drift | CORE-034, ARCH-079 |
| Agent edit | A change an AI agent made to a candidate or sealed file, recorded as AI work and never as a human edit. | human edit, hand edit | CORE-034, roadmap row 4b (attribution) |
| Claim | Your hold on an asset or document you opened to edit: the director doesn't write to it, and it lasts until you hand it back. | lock, lease (a checkout lease is another thing), hold (a document hold is APP-103's) | roadmap N31 |
| Hand back | Your action that ends a claim, so the director may work on that asset again. | release, unlock | roadmap N31 |
| Redirect | The record a renamed or moved asset leaves, so old references still resolve. | alias, symlink | ARCH-125, CORE-153 |
| Recipe | What a candidate records about how it was made: profile, model hashes, seed, prompt and settings. | provenance (the dropped C2PA sense), workflow | IMG-140, CORE-018 |
| QA gate | A technical, data-driven check per asset type; it blocks only automatic and agent sign-off. | licence gate, content filter (neither exists) | CORE-132, CORE-172 |
| Repair | A logged deterministic or model-driven fix applied to a candidate, always with a reason. | auto-fix | CORE-172, CORE-025 |
| Document | A hand-edited working file in the LILITH document format: a folder in a workspace, or a `.lildoc` outside one. | asset (a document is not versioned by sign-off), canvas file | APP-083, APP-085, CORE-137 |
| Proposal | A staged change to a document, such as an inpaint result, that you accept or discard. | candidate | CORE-043, APP-057 |
| Proposal run | One generation for a document, kept under the document's `proposals/<run_id>/`, separate from candidate runs. | candidate run | CORE-043 |
| Library resource | A typed reusable resource (swatch, palette, preset, model profile, workflow) in the workspace or personal scope. | library item, asset | APP-078, APP-079, APP-080 |
| Library item | A promoted asset in `library/<kind>/`, referenced with `lib:` links (Later). | library resource | Appendix E, workspace format I7 |
| Inbox | The workspace's `inbox/` quarantine where outside files wait for review; from plan 4b it receives the owned ComfyUI's new images. Nothing in it is an asset until you keep it. | staging, candidates | CORE-179, workspace format I1 |

## Tools, callers and safety

| Term | Meaning | Avoid | Defined in |
|---|---|---|---|
| Tool | A typed core operation (for example `image.generate`) that apps, `lil`, MCP clients and the assistant call through one dispatcher. | command, endpoint, action | ARCH-003, ARCH-037 |
| Caller kind | Who is calling: exactly one of `ui:<app>`, `assistant`, `mcp` or `cli`, decided by the core; it decides confirmations and permissions. | client type, role | ARCH-045, ARCH-047 |
| Agent caller | A caller of kind `assistant`, `mcp` or `cli`, as opposed to your own actions in an app (`ui:*`). | bot, AI user | ARCH-052, CORE-109 |
| Effect class | A tool's `read`, `write`, `destructive` or `exec` class, which decides whether an agent's call needs your confirmation. | permission level, risk level | ARCH-051 |
| Egress class | A tool's `none`, `own_provider`, `allowlisted_host` or `arbitrary` class: where its data may go. | effect class | AI-219 |
| Confirmation | Your yes or no to an agent's `destructive` or `exec` call (or an egress or large-expansion call), answered only in an app or Claude Code's own prompt. | approval, permission prompt (Claude Code's own), sign-off | ARCH-052, ARCH-053, ARCH-055, CORE-109 |
| Cap confirmation | A confirmation that lets a call go over a spending cap. | cap override | ARCH-053, ARCH-055, APP-054 |
| Confirm window | The minimal `lil-image --confirm` window that answers confirmations without the shell. | dialog (alone), popup | APP-048, APP-049 |
| Retry token | The token a `pending_confirmation` result carries; a retry with it re-evaluates the stored confirmation, and it is the call's idempotency key. | confirmation id | CORE-017, CORE-016 |
| Idempotency key | The key that makes the same call with the same arguments give the same job, result or confirmation. | request id | CORE-016 |

## Jobs, engines and the GPU

| Term | Meaning | Avoid | Defined in |
|---|---|---|---|
| Job | A unit of asynchronous work the core records, schedules and recovers, with a kind, status and substatus. | task (unless it is a director task), run | CORE-117, ARCH-020 |
| Director task | A job of kind `assistant_task` that MAGI Core plans and runs, started only from your click on the Start dialog. | task (alone), mode, session | CORE-118, AI-041, AI-213 |
| Owner stop | A point in a director task where it waits for your decision; a game has one per feature, where you play what was built and approve the next. | gate (alone), approval gate, QA gate (a technical check) | roadmap N32 |
| Outside job | A job in ComfyUI's queue that the suite didn't submit (from its web UI, the comfy MCP or scripts); observed, never cancelled. | foreign job | ARCH-069 |
| Lane | A scheduling queue for jobs: `gpu-heavy` (holds the GPU lease) or `cpu`. | queue, channel | CORE-121 |
| Priority class | The order across lanes: interactive operations, then your generations, then agent and director batches. | priority (alone) | CORE-124 |
| Pause GPU work | Holding every queued job of a lane, yours and agents', without stopping the running prompt. | suspend, stop | CORE-160 |
| Pause reason | A named cause that holds queued jobs, such as your own pause or the fullscreen auto-pause, each with the classes it covers. | pause type, pause flag | CORE-160 |
| Restart hold | Keeping a queued job that never reached an engine waiting after a core restart, instead of failing it, until you release it. | hold (alone: a document hold is APP-103's), pause | ARCH-022, CORE-160 |
| GPU lease | What a `gpu-heavy` job holds while it runs: one heavy GPU job at a time. | lease (alone), lock | ARCH-071, CORE-121 |
| Power request | What the core holds so the PC doesn't sleep while a GPU job runs. | wake lock, keep-awake lock | ARCH-127 |
| Checkout lease | A later record (holder, purpose, expiry) that checks a file out to an editor or tool, persisted in the manifest's `leases` field. | lease (alone), lock, hold | ARCH-092, CORE-036, ARCH-020 |
| Engine | An external program the core drives (ComfyUI, the Claude Code CLI, a local LLM server); for the assistant, also Engines B and C, the core's own turn loop over a provider's API. | backend | §1.4, ARCH-008, AI-199 |
| Owned engine | An engine the core started, and may health-check, restart and idle down. | managed engine | ARCH-064, ARCH-065, ARCH-126 |
| Attached engine | An engine someone else started; the core uses and observes it but never stops it. | external engine | ARCH-064, ARCH-065 |
| Engine A / B / C | The assistant's engines: A is the unmodified `claude` CLI on your own sign-in, B is Claude through the Anthropic API, C is any `openai_chat` route, hosted or local. | backend, model | AI-003, AI-047, AI-199 |
| Game engine | A program that builds and runs games, such as Unity or Godot, which the suite exports to. | engine (alone: the suite's engines are ComfyUI and the like) | APP-162, roadmap N38 |
| Engine target | One game engine and version, with its settings, that a project exports to. | platform, build target | APP-162, roadmap N38 |
| Engine adapter | The suite's code and capability table for one game engine and version, which writes and checks its exports. | plugin, bridge, exporter (alone) | roadmap N38 |

## AI, providers and money

| Term | Meaning | Avoid | Defined in |
|---|---|---|---|
| Provider | A hosted service the suite calls and meters, described by a provider profile. | engine, vendor | AI-007, CORE-101 |
| Route | One way to reach a model: a provider profile plus a model id (direct, OpenRouter, NanoGPT, or local). | provider (one provider has many routes), path | AI-034 |
| Included route | A route that costs nothing per call: Claude Code on a Pro/Max sign-in, or models a NanoGPT subscription covers. | free route | CORE-171 |
| Capability tier | A route's tool level, `chat`, `safe_tools` or `planner`, which decides which surfaces it can serve. | tier (alone; service tiers and CI tiers differ) | AI-208 |
| Account | Your account with a provider: key, billing basis, balances and quotas. | profile | APP-122, §4.3 |
| Usage record | One metered call or job's line in the usage logs: tokens, cost, caller, route and energy. | ledger entry, billing line | CORE-069, CORE-071 |
| Spending cap | An optional suite-level limit on spend, per month, per provider or per call, that agents can't change. | budget, quota, limit (provider-side limits differ) | ARCH-056, CORE-085 |
| Reservation | The upper-bound cost a call holds against the caps until it settles to the actual cost. | hold, charge | CORE-086, CORE-088 |
| Cost and time chip | The estimate shown before an action that spends GPU time or money. | price tag, quote | APP-140, CORE-161 |
| ORACLE | The general chat surface, which never acts and makes no tool calls. | co-pilot, assistant mode | AI-200, AI-003 |
| TETHER | The tool-using assistant inside a studio, a level of its Assistant panel. | co-pilot (its former name) | AI-039, AI-003 |
| MAGI Core | The director: a separate runner and surface that turns a goal into a director task. | director mode, agent mode | AI-041, AI-212 |
| Surface / level | ORACLE, TETHER and MAGI Core are surfaces; ORACLE and TETHER are also the fixed levels a conversation has. | mode | AI-003, AI-054 |
| Conversation store | The suite's record of assistant conversations, the source of truth over Claude Code's own transcripts. | chat history, session log | AI-069, AI-087 |
| Brief | Prompt Studio's model-neutral, versioned record of what a prompt should achieve: a `prompt_brief` asset. | spec, prompt | AI-159, AI-157 |
| Hand-off brief | The summary of a proposal's goal, outline and refs that a director task starts from (never the raw transcript). | brief (alone), transcript | AI-041, AI-213 |
| Compile | Rendering a brief into one target model's prompt, negative and settings hints. | generate, translate | AI-174, AI-157 |
| Prompt dialect | The form a model wants its prompt in (sentences, tags or an edit instruction), declared by its model profile. | prompt style, syntax | IMG-173, §5.6a |

## Profiles and presets

| Term | Meaning | Avoid | Defined in |
|---|---|---|---|
| Claude Code profile | One Claude Code account's config folder (`CLAUDE_CONFIG_DIR`), chosen by you per conversation or task, never by the suite. | profile (alone), account | AI-088, AI-089, AI-105 |
| Engine A launch profile | The fixed flags, system prompt file and permission rules one kind of Claude Code run starts with (TETHER, director, ORACLE's `chat`, interview). | launch profile (alone) | AI-050, AI-221 |
| ComfyUI launch profile | The recorded flags an owned ComfyUI starts with (`--reserve-vram`, previews and others). | launch profile (alone) | ARCH-126 |
| Model profile | Data on top of a model adapter: model files by hash, defaults, capabilities, required nodes and the prompt dialect; a Library resource. | preset, model card | IMG-099, AI-035 |
| Target profile | The model profile a brief is compiled for. | model profile (when the compile target is meant) | AI-174, AI-177 |
| Provider profile | A provider's declared endpoints, auth style, capability and quirk flags and usage-field mapping. | profile (alone), account | AI-007 |
| Hardware profile | The machine's measured hardware, compared with the workspace's `hardware` settings on core start. | profile (alone) | CORE-067 |
| Style preset / prompt preset | Library resources holding a reusable style (prompt, LoRAs and settings) or prompt. | preset (alone), template | APP-079 |
| Layout preset | An app-level file holding a window and dock layout. | preset (alone), workspace | APP-002, APP-004 |
| Permission preset | The Claude Code allow and deny rules an Engine A run gets (director or TETHER). | preset (alone), mode | AI-058, AI-059, AI-060 |
| Destination preset | A ready-made backup or S3 destination entry (endpoint and defaults) offered by "Add destination". | preset (alone), template | OPS-058, OPS-086 |
| Export preset | A Later Library resource kind holding export settings. | preset (alone) | APP-081 |

## Storage and backup

| Term | Meaning | Avoid | Defined in |
|---|---|---|---|
| `store` (module) | The one core module that writes workspace files. | storage, the large-file store | ARCH-002, ARCH-112 |
| Large-file store | Where Git LFS objects live besides this PC: none ("This PC only"), an external or second drive, or S3-compatible cloud (M1.5). | store (alone), destination | §7.2, OPS-008 |
| Destination | A backup target, a restic repository: the local primary drive plus optional cloud destinations. | store, remote | OPS-001, OPS-058 |
| Snapshot store | The per-workspace content-addressed store of a document's rolling autosave snapshots. | version, backup | APP-090, ARCH-097 |
| Chunk store | Where a document's layers live: content-addressed chunks of tiles. | tile cache | IMG-024, IMG-028 |

## Planning and records

| Term | Meaning | Avoid | Defined in |
|---|---|---|---|
| Rule id | A spec requirement's stable id (`CORE-042`), never renumbered or reused. | requirement number | §0.3 |
| Milestone | A delivery stage: `[M0]` spikes, then `[M1]` (with `[M1a]` and `[M1b]`), `[M1.5]` and `[Later]`. | release, phase | §0.3, Appendix A |
| M0 spike | A pass/fail experiment that must pass before the M1 work that depends on it. | prototype, POC | Appendix B |
| Plan | A numbered implementation plan in `docs/superpowers/plans/`, written only when it is next. | sprint, milestone | roadmap |
| Ruling | A decision made while a plan runs. | decision (a `(decision)` rule is binding spec text) | QA-090 |
| Decision record | A short file in `docs/decisions/` about one cross-cutting decision. | decision log, ledger | §0.3, `docs/decisions/README.md` |
