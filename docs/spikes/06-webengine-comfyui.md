# 06 WebEngine ↔ ComfyUI frontend scripting

Date: 2026-10-12

GPU: NVIDIA GeForce RTX 5080 16 GB, driver 610.88. Qt 6.12.0 + Qt WebEngine 6.140.0. ComfyUI 0.39.2, frontend `comfyui-frontend-package` 1.53.10, workflow templates 0.11.78.

## 1. Question

- **IMG-159:** do the frontend's `loadGraphData`, `graph.serialize()` and `graphToPrompt` work when driven from QtWebEngine? Can a graph be loaded into a new workflow tab? Does a hidden conversion page change ComfyUI's saved state? The last answer decides IMG-124's fallback.
- **IMG-127 / IMG-129:** what are the exact JavaScript entry points and call shapes for "Open in ComfyUI" and "Send to Worldwright" with frontend 1.53.10?
- **ARCH-070:** can the embedded view be locked down: navigation pinned to the origin, downloads blocked, popups handled?

## 2. Setup

| Item | Version |
|---|---|
| OS | Windows 11 Pro 10.0.26200 |
| GPU / driver | NVIDIA GeForce RTX 5080 16 GB, 610.88 |
| Qt / Qt WebEngine | 6.12.0 `msvc2022_64` / 6.140.0 (Chromium 140 base) |
| MSVC / CMake / Ninja | cl 19.44.35227 / 4.4.4 / 1.13.2 |
| KDDockWidgets | 2.4.1 (FetchContent, Qt Quick frontend) |
| ComfyUI | 0.39.2 at `%USERPROFILE%\ComfyUI\ComfyUI`. The check script started it on `127.0.0.1:8188` and stopped it again. |
| Python / PyTorch (ComfyUI venv) | 3.12.11 / 2.11.0+cu128 |
| ComfyUI frontend | `comfyui-frontend-package` 1.53.10. The calls were read from its bundled `static/assets/settingStore-CbkuaYBI.js`, `GraphView-DGi6q05V.js` and `downloadUtil-DCzQXle3.js`. |
| ComfyUI saved state | `%USERPROFILE%\ComfyUI\ComfyUI\user\default\`. The plan's path, `%USERPROFILE%\ComfyUI\user\default\`, does not exist, because ComfyUI's root is `ComfyUI\ComfyUI`. |
| Spike code | `spikes/webengine/`, in the commit that adds this document (parent 292722c) |

## 3. Method

### What was built (`spikes/webengine/`)

**The app (`webengine.exe`).**
- It is the Task 2 app cut down to one KDDockWidgets dock, "ComfyUI", which `Loader`-creates `qml/WebDock.qml`.
- There is no Rust crate: only the counter dock used cxx-qt, and that dock is gone.
- The shim (`spike_shim.dll`) contains `SpikeLog` (copied from Task 2) and `PageDriver`.
- The shim links `Qt6WebEngineCore` and `Qt6WebEngineQuick`, so this spike does not keep WebEngine off the startup path. APP-137 was proven in Task 2, and M1 needs the web half in a lazily loaded library (§8).

**`PageDriver` (C++, exposed to QML as the singleton `SpikeShim.PageDriver`):**
- `loadGraph(base64Json) → requestId`: fires `js/load_graph.js` in the visible view and does not wait.
- `readGraph() → string`: runs `js/read_graph.js`. The caller is blocked in a nested event loop for at most 5 s.
- `convertHidden(base64Json) → string`: runs `js/convert.js` in the hidden page, then polls the page's flag and takes the result. It waits at most 60 s, which includes the hidden page's first boot.
- `pollFlag(name) → string`: returns `window.__wwFlags[name]` from the visible view. It waits at most 1 s and returns `""` if the flag is unset or the page does not answer.
- `downloadAttempts: int`.
- The plumbing: `attachView`, `deliver(id, result)`, `allowNavigation(url)`, plus test helpers (`runInView`, `runInHidden`, `exportFromView`, `hiddenNavigationTest`, `sleep`, file I/O, base64, sha256).

**Visible view (`qml/WebDock.qml`).**
- A QML `WebEngineView` with `profile: PageDriver.viewProfile`.
  - That profile is a named persistent `QQuickWebEngineProfile` (`worldwright-spike-comfyui`).
  - Its `persistentStoragePath`, `cachePath` and `downloadPath` are all under `spikes/webengine/build/profile/`.
  - The paths are set before `storageName`. Constructing the profile with its name first makes WebEngine create an empty `%APPDATA%\<app>\QtWebEngine\<name>` folder; this was seen once and the empty folder was removed.
- PageDriver reaches the page through the view's QML function `run(id, script)`, which calls `view.runJavaScript(script, cb)`. The callback hands the result back with `PageDriver.deliver(id, result)`.
- Lock-down:
  - **navigation:** `onNavigationRequested` calls `PageDriver.allowNavigation(url)`, which allows only `http://127.0.0.1:8188`, `about:blank`/`about:srcdoc` and same-origin `blob:`. Anything else gets `request.reject()`.
  - **popups:** `onNewWindowRequested` is logged and never `openIn()`'d, so popups are refused.
  - **dialogs:** `onJavaScriptDialogRequested` accepts `beforeunload` and rejects other JS dialogs. `onFileDialogRequested` rejects.
  - **downloads:** the profile's `downloadRequested` is connected in C++ and calls `cancel()`.

**Hidden conversion page.**
- A C++ `QWebEnginePage` subclass that is **never attached to any view**. It is the Qt Quick-app counterpart of the plan's "second `QWebEngineView`, never shown".
- It has its own `QWebEngineProfile` built with no storage name, which makes it off-the-record.
- `acceptNavigationRequest` is overridden to call `allowNavigation`.
- `javaScriptConsoleMessage` is overridden to log warnings and errors.
- Its profile's `downloadRequested` cancels downloads too.
- It is created on the first `convertHidden` and loads the ComfyUI origin.

**Page scripts (`js/`, embedded as `:/js/*.js`).**
- Each file is one function expression.
- PageDriver builds the script as `(\n<file>\n)("arg1","arg2");`. Every argument must fully match `[A-Za-z0-9+/=:._-]*`, anchored with `QRegularExpression::anchoredPattern` (`\A(?:…)\z`). The arguments are base64, request ids, command ids and URLs, so no argument can leave its string literal.
- Graph JSON never becomes script text. It travels as base64 and is decoded in the page with `atob` + `TextDecoder('utf-8', {fatal: true})` + `JSON.parse`.

**Fixtures (`fixtures/`, made by `fixtures/make_fixtures.py`):**
- `zimage_t2i.ui.json`: the built-in template `image_z_image_turbo.json`, byte for byte. This is the same UI JSON that Task 4 converted. It already contains a subgraph: node 57, "Text to Image (Z-Image-Turbo)", with 9 inner nodes.
- `subgraph_muted.ui.json`: the same template with:
  - seed 1234 (inner KSampler 3), so its API prompt differs from fixture 1's;
  - a **muted** `PreviewImage` (node 62, mode 2) at the top level, fed from the subgraph output;
  - a **muted** `LatentUpscaleBy` (node 63, mode 2) inside the subgraph, fed from KSampler 3.

  These edits mirror what the 1.53.10 frontend serialized when the same nodes were added through LiteGraph in a browser.

**Check script (`run-checks.ps1`).** It builds, deletes the spike's own profile folder, and starts ComfyUI if it is not already up. Then it runs the phases below (each a separate app run) and two controls, and writes `results/webengine.json`:
1. **hidden:** the visible view stays on `about:blank`, so only the hidden page talks to ComfyUI. The hidden page runs 10 conversions, alternating the two fixtures, each stamped as in criterion 2. It also tries to navigate to `https://example.com`. `user\default` and the whole `user\` folder are snapshotted (file list, sha256 and mtime per file, plus the text of `comfy.settings.json`) **immediately before and after this run only**.
2. **visible:** for each fixture:
   - add `extra.worldwright = {source_ref, base_hash}`;
   - `loadGraph`, polling the flag every ~25 ms;
   - read the tab state before and after;
   - `readGraph`;
   - run the frontend's own **Export (API)** command. `js/export_capture.js` wraps `URL.createObjectURL` once to copy the blob text, confirms the filename prompt with Enter, and the download is then cancelled by C++.

   After the fixtures, the phase runs one plain **Export**, then one hidden conversion while the view is open (the view's tabs are compared before and after). Last, it tries `location.href = "https://example.com/"` and `window.open("https://example.com/")`. The user's Downloads folder is listed before and after; only the names of files that are new or rewritten would be recorded.

   Before the fixtures, a **negative probe** calls `loadGraph` with five hostile arguments:
   - `QUJD"+alert(1)+"` (quote);
   - `QUJD\` (backslash);
   - `<script>QUJD` (`<`);
   - a raw JSON string `{"nodes":[],"links":[]}`;
   - `QUJD` + a trailing `\n`.

   It records the view's tabs before and after, and the flag for each request id.
3. **down:** after stopping the ComfyUI it started, the script runs `loadGraph` against Chromium's error page.
4. **firstboot** (informational, for IMG-124):
   - The script starts a **scratch ComfyUI** on `127.0.0.1:8189` with `--user-directory build\scratch-comfy\user` and `--temp-directory build\scratch-comfy\temp`. Both are fresh, empty folders under the spike's build folder.
   - It runs the app twice with `--origin http://127.0.0.1:8189`. Each run does one hidden conversion; the visible view stays on `about:blank`.
   - It snapshots the scratch user folder before ComfyUI starts, once ComfyUI is up, and after each run.
   - It stops that process afterwards. The real user folder is not involved.
5. **controls** (recorded in `details.controls`):
   - The snapshot/diff code is run on a scratch folder under `build\`. A settings file is edited and a workflow file added; the diff must report both, and an unchanged re-snapshot must be identical.
   - The real `user\` folder is snapshotted before the script starts any ComfyUI and again at the very end.

Every folder the script deletes (`build\profile`, phase output folders, `build\scratch-comfy`, `build\snapshot-control`, `build\cmake` with `-Clean`) goes through `Remove-BuildDir`. That function throws unless the full path is under `spikes\webengine\build\`.

Re-run:

```powershell
pwsh -File spikes/webengine/run-checks.ps1          # build + all phases + results/webengine.json
spikes/webengine/build/cmake/bin/webengine.exe      # no --phase: just shows ComfyUI (Task 8)
```

## 4. Pass criteria

Copied verbatim from the plan (Task 6, step 1) before the spike was run:

1. `loadGraph` opens the fixture **in a new workflow tab** without replacing the user's current tab, and the flag reads `ok` within 3 s. The graph is passed as base64 and decoded in the page; there is no string-built script.
2. `readGraph` returns JSON whose node ids and links equal the loaded fixture, and an `extra.worldwright` stamp added before loading survives the round-trip (IMG-128).
3. **Hidden conversion page:** a second `QWebEngineView`, never shown, with its own off-the-record `QWebEngineProfile`, loads ComfyUI and converts both fixtures. Its `output` equals what ComfyUI's own "Export (API)" produces for the same fixtures, compared as JSON after sorting keys.
4. **State isolation:** the hidden page leaves ComfyUI's saved state alone. Snapshot `%USERPROFILE%\ComfyUI\user\default\` (file list, hashes, and `comfy.settings.json` content) before and after 10 hidden conversions; the snapshots must be identical. If they aren't, record exactly which files changed. That decides IMG-124's fallback ("Run in ComfyUI tab").
5. **Downloads blocked:** triggering a download in the page (ComfyUI's "Export" → file) is cancelled in `QWebEngineProfile::downloadRequested`, `downloadAttempts` increments, and no file appears in the user's Downloads folder.
6. **Navigation pinned:** navigating to `https://example.com` from the page is refused (the `acceptNavigationRequest` override), and the view stays on `127.0.0.1:8188`.
7. **Negative check:** with ComfyUI stopped, `loadGraph` sets the flag to `error:unreachable`, or no flag appears and the call times out after 5 s with a clear log line. The app doesn't hang.

**How the script reads them.**
- **1:** the positive part:
  - The flag is `ok` within ≤ 3000 ms of the call.
  - There is exactly one more open tab, and every tab that was open before is still open.
  - The new active tab is not one of the old ones, and the previously active tab's node/link shape hash is unchanged.
  - PageDriver's own `load_graph_call` log line shows `script_ok: true` and `sent: true`, with `base64_chars` equal to the base64 length of the stamped fixture bytes. The script computes that length independently from the file.

  The negative probe must also pass, for all 5 hostile arguments:
  - the log line just before that request's `load_graph_call` is `driver_error {what: "argument rejected", script: "load_graph.js"}`;
  - `load_graph_call` has `script_ok: false` and `sent: false`;
  - the page never shows a flag for that id;
  - the view's tabs are identical before and after the probe.
- **2:** compared per graph: sorted node ids and links at the top level, and the same for every subgraph definition. The stamp must be deep-equal.
- **3:** the API JSON is canonicalized (keys sorted recursively) and compared as text. All 5 conversions of a fixture must also give one identical output.
- **4:** checked on the real `user\default` path (§2).
- **5:** every export trigger must produce exactly one `download_blocked` event with state `cancelled`, the counter must increment by one, no file may be new in `%USERPROFILE%\Downloads`, and the profile's own `downloads/` folder must stay empty.
- **6:**
  - The visible view is a Qt Quick `WebEngineView`. It has no `acceptNavigationRequest` to override, so its pin is the `navigationRequested` handler, the QML counterpart.
  - The C++ `acceptNavigationRequest` override is tested on the hidden page.
  - Both must refuse the navigation and stay on the origin. The popup must be refused too.

## 5. Measurements

Final run (after review fix round 1): `run-checks.ps1`, results written at 2026-10-12 18:16:00, exit 0, `ALL CRITERIA PASSED`.
- Earlier full runs: 17:59 and 18:01 (before the review; all criteria passed under the old, weaker criterion-1 check), and 18:12 (criterion 1 FAIL from the negative probe, see below).
- The timings in all of them were similar: flags at 99–178 ms, warm conversions 89–172 ms, first conversion 2.3 s.

Raw data: (raw output kept privately), including the converted, exported and read-back JSON. The window grabs (Qt `grabWindow`, own window only) are committed as `spikes/webengine/results/*.png`.

| Metric | n | p50 | p95 | p99 | max | Unit |
|---|---|---|---|---|---|---|
| `loadGraph` call → flag `ok` (visible view) | 2 | 108 | 178 | 178 | 178 | ms |
| Hidden page created → frontend ready (`app_ready.js`) | 1 | 2117 | 2117 | 2117 | 2117 | ms |
| `convertHidden`, first call (includes the page boot) | 1 | 2326 | 2326 | 2326 | 2326 | ms |
| `convertHidden`, warm (calls 2–10) | 9 | 105 | 163 | 163 | 163 | ms |
| ComfyUI down: `loadGraph` → `error:unreachable` | 1 | 2035 | 2035 | 2035 | 2035 | ms |
| ComfyUI down: GUI heartbeat gap (250 ms timer) after the call | 20 | — | — | — | 266 | ms |

Per criterion:

1. **PASS.**
   - The tabs before were `Unsaved Workflow.json` (the frontend's default tab in the fresh profile). After the first load they were `Unsaved Workflow.json`, `Unsaved Workflow (2).json`; after the second, `… (3).json` was added.
   - Every old tab was kept, its shape hash was unchanged, and the new tab was active.
   - The flag was `ok` after 178 ms and 108 ms.
   - `load_graph_call` showed `script_ok: true` and `sent: true`, with `base64_chars` 17,472 and 17,400. Both equal the base64 length of the stamped fixture files.
   - **Negative probe:** all 5 hostile arguments were refused. Each had `argument rejected` logged, `script_ok: false`, `sent: false` and no flag in the page, and the tabs (`Unsaved Workflow.json`) were unchanged.
   - **What "no string-built script" means here.**
     - Taken literally, the plan's sentence is not true: PageDriver does build the script text, `(<js/load_graph.js>)("<arg>","<requestId>");`.
     - The only data spliced in are string literals that passed the allowlist `\A[A-Za-z0-9+/=:._-]*\z`, which cannot end a double-quoted JS string. The graph itself is a base64 literal, decoded and `JSON.parse`d inside the page.
     - So the IMG-127 invariant holds: no graph content can become script.
   - **The probe found a real bug.**
     - The first allowlist was `^[A-Za-z0-9+/=:._-]*$`. In `QRegularExpression`, `$` also matches before one final `\n`, so `QUJD\n` was **accepted and sent**: `script_ok: true`, `sent: true`.
     - The page stayed unchanged only because the raw newline made the script a syntax error.
     - That run failed criterion 1 (raw output kept privately).
     - The fix uses `QRegularExpression::anchoredPattern(...)` (`\A(?:…)\z`) for the argument and script-name patterns.
2. **PASS.**
   - The top-level nodes `[35, 57, 9]` (fixture 2: `[35, 57, 62, 9]`), the links, and the subgraph `f2fdebf6-…`'s 9 nodes and 18 links (fixture 2: 10 nodes and 19 links) are all identical in the `readGraph` output.
   - `extra.worldwright = {source_ref: "spikes/webengine/fixtures/<name>.ui.json", base_hash: "sha256:…"}` came back unchanged.
3. **PASS.**
   - Hidden output equals the visible tab's Export (API) after key sorting, for both fixtures. Canonical sha256: `B9180BC8EE3F…` (fixture 1) and `FE022418AE44…` (fixture 2).
   - All 5 conversions per fixture gave one identical output.
   - Fixture 2's API prompt has no node `62` and no `57:63`, so the muted nodes were dropped. Its `57:3` seed is 1234.
   - Fixture 1's output is also identical to Task 4's `spikes/comfy-measure/workflows/zimage_t2i.json`.
4. **PASS (identical).**
   - Before and after 10 hidden conversions: `user\default\` held the single file `comfy.settings.json` (125 bytes), with the same sha256, the same mtime and the same content (`Comfy.InstalledVersion: 1.53.10`, `Comfy.TutorialCompleted: true`, `Comfy.Templates.SortBy: popular`).
   - Diff: no files added, removed or changed, and no mtime-only touches. The whole `user\` folder (including `__manager\`) is identical too.
   - The visible phase, which included 2 tab loads, 3 exports and one more hidden conversion with the view open, also changed nothing on disk. The view's tabs were identical before and after that hidden conversion.
   - **Controls** (`details.controls` in `webengine.json`):
     - `snapshot_diff_detects_change.pass = true`: on a scratch folder, the same code reported the edited `comfy.settings.json` (changed, settings content not equal) and the added `workflows\x.json`. An unchanged re-snapshot was identical.
     - `real_user_folder_whole_run.identical = true`: the real `user\` folder was identical from before the script started any ComfyUI to the end of the script.
   - **The limit of this result.** It was measured on a ComfyUI whose `comfy.settings.json` already existed from an earlier first visit (Task 4's browser pane). The first-boot behaviour was therefore measured separately on a scratch ComfyUI (next item).
   - **First boot, measured** (`details.first_boot`; scratch ComfyUI on port 8189 with its own empty `--user-directory`):
     - Starting the ComfyUI server wrote nothing to the user folder.
     - **The hidden page's first boot created `default\comfy.settings.json` (125 bytes)** with exactly:

       ```json
       {
           "Comfy.InstalledVersion": "1.53.10",
           "Comfy.TutorialCompleted": true,
           "Comfy.Templates.SortBy": "popular"
       }
       ```

       No other file was written: no workflows, no database.
     - The second app run (a new off-the-record page, so empty `localStorage` again) changed nothing.
     - The scratch process was stopped (`scratch_stopped: true`).
     - These are the same three keys, with the same values, as the real `comfy.settings.json`. The real file was evidently created the same way, by the first frontend visit to this ComfyUI.
5. **PASS.**
   - 3 export triggers (Export (API) ×2, Export ×1) gave 3 `download_blocked` events: profile `view`, `blob:` URL from the ComfyUI origin, `application/json`, suggested names `Unsaved Workflow (2).json` / `(3).json`, state `requested` → `cancelled`.
   - `downloadAttempts` went 0 → 1 → 2 → 3.
   - The user's Downloads folder had 71 files before and after, with nothing new or rewritten. The profile's `downloads/` folder is empty.
6. **PASS.**
   - **Visible view:** `navigation {url: https://example.com/, main_frame: true, allowed: false}`. The load reports `stopped net::ERR_ABORTED`, and `view.url` stays `http://127.0.0.1:8188/#…`. Afterwards `location.origin` is `http://127.0.0.1:8188` and the app is still alive.
   - **Popup:** `window.open` raised `new_window {destination: 1 (NewTab), opened: false}`. No window was opened. The page's `window.open` still returned a WindowProxy object, which is Chromium behaviour for an unhandled new-window request.
   - **Hidden page:** the `acceptNavigationRequest` override returned false for `https://example.com/`. Its URL stays on the origin and its frontend is alive.
7. **PASS.**
   - The view failed with `net::ERR_CONNECTION_REFUSED` (error page).
   - `load_graph.js` still ran on Chromium's error page: it found no `window.app` within 2 s and set `error:unreachable` (2035 ms after the call).
   - 20 heartbeats after the call, max gap 266 ms; `Process.Responding` was never false; exit 0 in 8.5 s.
   - The script also accepts the other outcome the criterion allows (no flag and a `load_graph_timeout` log line at 5 s), but that path was not needed.

RED:
- Step 2: `run-checks.ps1` before `PageDriver` existed gave `criterion 1 : FAIL` ("no PageDriver: …PageDriver.cpp / …CMakeLists.txt not present") and exit 1 (raw output kept privately).
- Review fix round 1: the negative probe with the old `^…$` allowlist gave `criterion 1 : FAIL` (the `trailing_newline` case was sent) and exit 1 (raw output kept privately).

**"ComfyApp graph accessed before initialization".** Both pages log this `console.error` once per boot. It is a benign boot race inside the frontend, not something our scripts cause:
- The message comes from `ComfyApp.get rootGraph()` when `rootGraphInternal` is still unset.
- `app.setup()` runs `await useExtensionService().loadExtensions()` *before* it assigns `this.rootGraphInternal` and `this.canvas`, so an extension touching `app.rootGraph` during loading trips it.
- In the visible run it appears at t = 1010 ms, before the check sequence starts (t = 1576 ms) and before PageDriver has sent any script to the page.

`app_ready.js` is still a sound readiness gate:
- `window.app` is assigned by GraphView only after `await app.setup()` has returned, and setup assigns `rootGraphInternal` and `canvas` together.
- The gate also requires `app.canvas` and `app.extensionManager.workflow.activeWorkflow`, so it cannot pass before the root graph exists and a workflow tab is open.

## 6. Verdict

**PASS.** All seven criteria pass.
- The 1.53.10 frontend can be driven from QtWebEngine with three entry points: `app.loadGraphData`, `app.graph.serialize` and `app.graphToPrompt`.
- A graph loaded this way opens in a new temporary workflow tab and leaves the user's open tabs alone. It reads back with ids, links and the `extra.worldwright` stamp intact.
- A viewless page with an off-the-record profile boots the frontend in about 2.1 s and converts in about 0.1 s.
- Its output is semantically equal JSON (after key sort) to the frontend's own Export (API).
  - The Export (API) reference also goes through `app.graphToPrompt` (in the visible tab). The comparison therefore catches differences that come from the hidden page, its profile or our load options.
  - It would not catch a bug in `graphToPrompt` itself. Fixture 1's output matching Task 4's API workflow, which ComfyUI accepted with no `node_errors` and ran, is the only external check.
- Ten hidden conversions left ComfyUI's saved state untouched.
- Downloads are cancelled in C++, off-origin navigation and popups are refused, and an unreachable ComfyUI gives a prompt `error:unreachable` flag without hanging the app.

## 7. Decision

**IMG-127: entry point and call shape (frontend 1.53.10).** The call is

```js
await window.app.loadGraphData(graph, true, true, null, { openSource: 'worldwright' })
```

- Signature: `ComfyApp.loadGraphData(graphData, clean = true, restore_view = true, workflow = null, options = {})`, where `options` = `{checkForRerouteMigration, openSource, shareId, deferWarnings, skipAssetScans, silentAssetErrors, workflowNavigationId}`.
- Returns `false` if the graph cannot be configured; otherwise it resolves truthy.
- **New tab:** with `workflow` = `null`, `workflowService.afterLoadNewGraph` → `workflowStore.createNewTemporary(undefined, graph)` + `openWorkflow`, which gives a new tab "Unsaved Workflow (n)".
  - A string `workflow` names the tab. But if a tab with that path is already active and temporary with the same workflow id, it is **reused**, so a fresh name or `null` should be passed for "always a new tab".
- `openSource` is only used for telemetry, and to `fitView()` when it equals `'template'`.
- Wiring:
  - The graph travels as base64 and is decoded in the page.
  - The async result goes to `window.__wwFlags[requestId]` (`pending` → `ok` | `error:<msg>`), which the suite polls with a one-line `runJavaScript`.
  - Readiness: `window.app` is set by GraphView after `app.setup()`. The test `window.app && app.canvas && app.extensionManager.workflow.activeWorkflow` means the frontend has booted.

**IMG-129: entry point.** `JSON.stringify(window.app.graph.serialize())` through a synchronous `runJavaScript`. In 1.53.10, `app.graph` is the root graph (`get graph() { return this.rootGraphInternal }`), so the result includes the `definitions.subgraphs` even while a subgraph is open. The result was 16–17 KB for these fixtures.

**IMG-124: hidden conversion is safe; the "Run in ComfyUI tab" fallback is not needed for saved-state reasons.**
- The call shape:

  ```js
  await app.loadGraphData(graph, true, false, null, { skipAssetScans: true, silentAssetErrors: true, deferWarnings: true })
  JSON.stringify((await app.graphToPrompt()).output)
  ```

  `graphToPrompt(graph = app.rootGraph)` returns `{workflow, output}` and honours `Comfy.Workflow.SortNodeIdOnSave`. This is the same function Export (API) uses: `Comfy.ExportWorkflowAPI` → `workflowService.exportWorkflow('workflow_api', 'output')` → `JSON.stringify((await app.graphToPrompt()).output, null, 2)`.
- It runs in a `QWebEnginePage` with no view and its own off-the-record `QWebEngineProfile`.
- Why it is safe: in 1.53.10 the open-tab state (`Comfy.OpenWorkflowsPaths`, `Comfy.ActiveWorkflowIndex`, `Comfy.Workflow.DraftIndex.v2:*`, drafts) lives in the browser's `localStorage`/`sessionStorage`, not on the server. An off-the-record page cannot reach the user's tab state.
- Measured evidence:
  - On a ComfyUI that had been opened before, 10 hidden conversions changed nothing in `user\`.
  - On a never-opened ComfyUI (the scratch first-boot probe), the first hidden boot wrote only the one-time `comfy.settings.json` keys (`InstalledVersion`, `TutorialCompleted`, `Templates.SortBy: popular`), and later boots wrote nothing.
  - The first-boot write is what any first browser visit writes. It touches no tabs and no workflows (§8, follow-up 2).

**ARCH-070 / IMG-126 (recorded, no spec change).** Lock-down works as specified:
- named persistent profile with storage redirected;
- navigation pin in `navigationRequested` (QML) or `acceptNavigationRequest` (C++);
- popups refused in `newWindowRequested`;
- downloads cancelled in the profile's `downloadRequested`;
- no web channel.

## 8. Follow-ups

1. **Make the web half lazy in M1.** This spike's shim links WebEngine, which breaks APP-137 if it is copied as is. Put `PageDriver` and the hidden page in a separate DLL (e.g. a QML plugin imported only by the ComfyUI panel), so nothing loads WebEngine before the panel is first shown.
2. **IMG-124 fresh-install caveat (spec note). Measured, not just source-derived.**
   - On a scratch ComfyUI with an empty `--user-directory`, the hidden page's first boot created `default\comfy.settings.json` holding `Comfy.InstalledVersion: "1.53.10"`, `Comfy.TutorialCompleted: true` and `Comfy.Templates.SortBy: "popular"`. A second boot wrote nothing.
   - Sources in the code:
     - `useNewUserService.initializeIfNewUser` writes `InstalledVersion`. It fires when the server settings are empty or `TutorialCompleted` is false, *and* `localStorage` holds no workflow; an off-the-record page always has empty `localStorage`.
     - `resolveStartupOutcome`/`markTutorialCompleted` writes `TutorialCompleted`.
     - `Templates.SortBy` was not traced in the source. It is most likely the templates store's debounced `sortBy` watcher persisting its default.
   - Every key is a one-time first-visit key. Any browser opening ComfyUI the first time writes the same three. None touches tabs or workflows.
   - Suggested IMG-124 wording: "hidden page, off-the-record; on a ComfyUI that has never been opened, the first frontend boot (hidden or not) writes the first-visit settings keys `Comfy.InstalledVersion`, `Comfy.TutorialCompleted` and `Comfy.Templates.SortBy`, like any first visit; nothing else".
3. **The hidden page's tabs accumulate.** Each conversion creates one temporary workflow in the hidden page's memory (10 here). M1 should close it after conversion or reuse a fixed tab name. Reusing the same name is fine there, because its contents are discarded anyway.
4. **`QQuickWebEngineProfile` construction from C++ is deprecated.** Qt 6.12 warns: "Please use WebEngineProfilePrototype for profile creation from 6.9". M1 should create the named profile with `WebEngineProfilePrototype` (QML) or check the C++ replacement.
5. **Frontend-version coupling.** The call shapes above are internal frontend API, not a public contract. The coupled parts are:
   - `loadGraphData`'s positional parameters and options object;
   - the `workflow` argument's "new temporary tab" behaviour;
   - `window.app` being set after setup;
   - `app.extensionManager.workflow` and `.command`;
   - Export going through `downloadBlob` (`<a download>`);
   - the reka filename prompt.

   QA-059's fake-ComfyUI test should pin these against the bundled frontend version, and the suite should check `window.__COMFYUI_FRONTEND_VERSION__` (it is `1.53.10` here) before scripting.
6. **Template migration warnings.** Loading the Z-Image template logs `[proxyWidgetMigration] source widget has no backing input slot; quarantining` (the template's `control_after_generate` proxy widget). The frontend adds `properties.proxyWidgetErrorQuarantine` to node 57 on load. The ids and links are unaffected, but IMG-129's "Update source" hash check will see a changed graph even when the user changed nothing. M1a (IMG-129 is [M1a]) should compare a normalized form, not raw text.
7. **Export (API) via the menu needs the filename prompt confirmed.** `Comfy.PromptFilename` defaults to true. The suite never needs Export, because it calls `graphToPrompt` directly, but QA tests that drive the menu must confirm the prompt.
8. **`%LOCALAPPDATA%\webengine_spike\cache\qmlcache`** (124 KB) is Qt's QML disk cache, as in Task 2. It is not a Worldwright folder.
