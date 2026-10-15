# 07 Clipboard and drag-out formats

Date: 2026-10-12

Status: all criteria PASS (3, 4, 5b, 6 per a manual-check summary). Criteria 1, 2 and 5a were measured by script; checks 3, 4, 5b and 6 were done by hand (section "Manual check results").

## 1. Question

- **APP-071, APP-068, APP-069:** which `QMimeData` recipe makes a copied or dragged image arrive as an image in Discord, Chrome and Paint, as a copy (never a move) in Explorer, and as plain text nowhere?
- The verified recipe is the input to plan 4's C++ shim (APP-068).

## 2. Setup

| Item | Version |
|---|---|
| OS | Windows 11 Pro 10.0.26200 |
| Qt | 6.12.0 `msvc2022_64` (Qt Quick, Qt Quick Controls 2) |
| MSVC / CMake / Ninja | cl 19.44.35227 / 4.4.4 / 1.13.2 |
| PowerShell | 7 for the scripts; Windows PowerShell 5.1 (STA) for `Get-Clipboard -Format` |
| Spike code | `spikes/clipboard/`, in the commit that adds this document |

No Rust crate, KDDockWidgets, WebEngine or GuiPrivate: the spike is a plain Qt Quick app plus a shim DLL.

## 3. Method

### What was built (`spikes/clipboard/`)

- **`assets/source.png`**: 256×256 RGBA, 2033 bytes, generated deterministically by `assets/make_source_png.py` (stdlib only). R = x, G = y, B = 255 − (x+y)/2, so a swapped channel is obvious. A half-transparent disc (alpha 96) sits in the middle, and the top-right 32×32 corner is fully transparent. It is embedded in the exe as a Qt resource.
- **`shim/ImageMime.{h,cpp}`**: the recipe (section 3.1). Pure Qt, no spike-specific code.
- **`shim/ClipboardBridge.{h,cpp}`**: QML singleton `Bridge`. `copyToClipboard()` puts the recipe on the clipboard. `startDrag(item)` runs a `QDrag` with the same mime data and `exec(Qt::CopyAction, Qt::CopyAction)`. Both log JSON lines through `SpikeLog` (copied from Task 2).
- **`qml/Main.qml`**: a Copy button, a 256×256 picture on a checkerboard (so transparency is visible) and a status line. A `MouseArea` starts the drag after 8 px of movement and resets itself afterwards, because the OLE drag loop swallows the button release.
- **`main.cpp`** switches: `--log <file>`, `--auto-copy`, `--selftest-drag`, `--quit-after <ms>`, `--export-dir <dir>`.
- **`inspect_clipboard.ps1`**: lists the clipboard formats (name, id, size) with a P/Invoke on `EnumClipboardFormats`, hashes the `PNG` format, and cross-checks with `Get-Clipboard -Format FileDropList/Text/Image`. Sizes, hashes and `Get-Clipboard` are used only when the clipboard holds this spike's own data (the custom format is present). For anything else only format names are listed and no data is read.
- **`run-checks.ps1`** (`-NoBuild`, `-Clean`): builds, deploys with `windeployqt`, runs the app with a PATH stripped of Qt, MSVC and Windows Kits, then evaluates criteria 1, 2 and 5a. It writes `results/clipboard.json`; the RED run and the full format list after Copy were written next to it (raw output kept privately).

### 3.1 The verified `QMimeData` recipe

```cpp
auto *mime = new QMimeData;
// 1. custom format: Qt registers a Windows clipboard format with this exact name
mime->setData("application/x-worldwright-ref", refJson.toUtf8());
// 2. standard bitmap: Qt serves CF_DIBV5 and CF_DIB (Windows adds CF_BITMAP)
mime->setImageData(QImage::fromData(pngBytes, "PNG"));
// 3. the exact PNG bytes under the Windows registered format "PNG"
mime->setData("application/x-qt-windows-mime;value=\"PNG\"", pngBytes);
// 4. file URL of the exported copy in %TEMP%\ww-spike-export\  -> CF_HDROP
mime->setUrls({QUrl::fromLocalFile(exportedPath)});
// 5. no setText(), no setHtml()
QGuiApplication::clipboard()->setMimeData(mime);            // clipboard takes ownership

// drag-out: same mime data, copy only
auto *drag = new QDrag(window);
drag->setMimeData(mime);
drag->exec(Qt::CopyAction, Qt::CopyAction);                  // supportedActions = defaultAction = Copy
```

Findings that shaped it:

1. A plain `setData("image/png", bytes)` does **not** reach the Windows format `PNG`. Qt's image converter writes its own `PNG` from the bitmap, re-encoded (1579 bytes, not the source's 2033), and puts the real bytes under a Qt-only format named `image/png`. The pseudo mime type `application/x-qt-windows-mime;value="PNG"` makes Qt's last-resort converter hand the bytes to `RegisterClipboardFormat("PNG")` untouched. With it, `PNG` is 2033 bytes with the source's hash.
2. `setUrls()` of a local file gives `CF_HDROP`, and also `UniformResourceLocatorW` and `UniformResourceLocator` (single URL). `FileNameW` is not produced; `CF_HDROP` is the file format Explorer, Discord and Chromium read. The URL formats are a thing to watch in the owner checks.
3. `setImageData` makes Qt advertise one `image/<ext>` clipboard format for every image writer (bmp, cur, icns, ico, jpeg, jfif, jpg, pbm, pgm, ppm, tif, tiff, wbmp, webp, xbm, xpm, png). They are rendered only when an app asks, but they clutter the format list. They cannot be trimmed with a `QMimeData::formats()` override, because Qt's Windows converter adds them below that layer.
4. No `CF_UNICODETEXT`, `CF_TEXT` or `CF_OEMTEXT` appears, even with a file URL in the data. Qt does not turn URLs into text on Windows.
5. `QDrag::exec()` blocks inside OLE `DoDragDrop`. With an idle mouse it never returns and does not run Qt timers (so `QDrag::cancel()` from a timer never fires). The self test ends it with a helper thread that posts an in-process `WM_MOUSEMOVE` to the app's active window every 500 ms; OLE then sees no button down and drops with no target (result `ignore`). This is not desktop input. Three consecutive runs gave the same result.

### How to re-run

```powershell
. spikes/toolchain/env.ps1
pwsh -File spikes/clipboard/run-checks.ps1            # build + deploy + criteria 1, 2, 5a
pwsh -File spikes/clipboard/inspect_clipboard.ps1     # after clicking Copy in the app
```

The exported copy goes to `%TEMP%\ww-spike-export\worldwright-spike-image.png` only.

## 4. Pass criteria

Copied from the plan before running.

1. `inspect_clipboard.ps1` after Copy lists `PNG`, `CF_DIB` or `CF_DIBV5`, `FileNameW` / `CF_HDROP`, and the custom format, and does **not** list `CF_UNICODETEXT`.
2. The PNG bytes on the clipboard are byte-identical to the source PNG (same SHA-256).
3. **Manual check:** pasting into Discord (desktop app) attaches the image, not text or a path.
4. **Manual check:** pasting into Chrome (e.g. a Google Docs page, or any page that accepts pasted images) inserts the image.
5. Dragging the thumbnail to an Explorer folder **copies** it. The exported source file still exists afterwards, and Explorer offers no "Move".
6. Pasting into Paint gives the image at full size and colour.

## 5. Measurements

No timing metrics. Results are pass/fail; raw data: (raw output kept privately).

Formats on the clipboard after Copy (own data only; `Get-Clipboard` cross-check: file drop list = the exported copy, text absent, image 256×256):

| Format | Size (bytes) |
|---|---|
| `application/x-worldwright-ref` | 110 |
| `PNG` | 2033 (sha256 `fd880fc1…8790`, equal to `assets/source.png`) |
| `CF_DIBV5` | 262280 |
| `CF_DIB` | 196648 |
| `CF_BITMAP` | synthesized by Windows |
| `CF_HDROP` | 178 |
| `UniformResourceLocatorW` / `UniformResourceLocator` | 172 / 85 |
| `image/png`, `image/bmp`, `image/jpeg`, … (17 Qt-only formats) | rendered on request |
| `DataObject`, `Ole Private Data` | OLE housekeeping |
| `CF_UNICODETEXT`, `CF_TEXT`, `CF_OEMTEXT` | absent |

RED (before the fix in finding 1): `PNG` was 1579 bytes, a different hash, so criterion 2 failed. A first RED for the whole script (no project present) was also recorded (raw output kept privately).

Criterion 5a (automated part of 5), from `build/runs/drag.jsonl`: a synthetic mouse press and move was sent to the app's own window (no desktop input). The drag code path ran: `QDrag` `supportedActions` = `[copy]`, `defaultAction` = `copy`, the offered formats are the recipe's four (no `text/plain`), and the exported file still existed afterwards with the source's hash. The drag result was `ignore` because there was no drop target, which is expected. The run also checks that the exported file is not read-only (APP-070): `IsReadOnly` = False.

## 6. Verdict

PASS. Criteria 1, 2 and 5a pass on the deployed exe started with a PATH stripped of Qt and MSVC. Criteria 3, 4, 5b and 6 pass per the manual-check report (section "Manual check results"). That report was a one-line summary, not per-item detail, so the evidence for 3, 4, 5b and 6 is weaker than for 1, 2 and 5a.

## 7. Decision

APP-068: the recipe in section 3.1 is confirmed as the `QMimeData` recipe for the shim (APP-071 clipboard, APP-069 drag-out), including the `application/x-qt-windows-mime;value="PNG"` pseudo type for the exact PNG bytes.

## 8. Follow-ups

- The two `UniformResourceLocator*` formats and the 17 `image/*` formats come from Qt and cannot be removed through `QMimeData`. No manual check showed a problem from them. If an app ever pastes a link instead of the image, the fallback is a small native `IDataObject` for the clipboard and drag.
- The shim must keep the exported copy alive for as long as its file URL may be on the clipboard or dragged.
- Drag-out cannot be tested without a real mouse; the self test only proves the code path and the action mask.

- Spike-level shortcuts that must be handled in the real shim: the `QDrag` created in `startDrag` is never deleted (Qt parents it to the window, so it lives until the window closes), and `inspect_clipboard.ps1` lists names in pass 1 and reads sizes in pass 2, so the clipboard can change between the two.
- `ImageMime::build` now logs a warning and skips the bitmap formats if the PNG bytes do not decode; the real shim should return an error instead.

## Manual check results

Date: 2026-10-12. Checks 3 to 6 were run by hand with `spikes/clipboard/build/deploy/clipboard_spike.exe`. The report was a one-line summary that everything worked and that Chrome showed the image. **This was a summary, not per-item detail:** the apps were not reported separately, so the results below are recorded as PASS per that summary. Only Chrome was mentioned by name.

| Check | Result | Note |
|---|---|---|
| 3 Discord paste attaches the image | PASS | per the summary |
| 4 Chrome paste inserts the image | PASS | Chrome was named as showing the image |
| 5b Explorer drag copies, source remains, no Move | PASS | per the summary |
| 6 Paint paste, full size and colour | PASS | per the summary |
| 5b optional Shift-drag | not reported | |

No failure was reported, so there is no "pasted instead" note. `run-checks.ps1` reads the manual-check record from `results/owner-checks.json` (raw output kept privately).


