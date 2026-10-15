#!/usr/bin/env python3
"""Keep ComfyUI's GPU busy for the canvas frame-time spike (Task 4: IMG-153, APP-135, IMG-040).

Submits spikes/comfy-measure/workflows/zimage_t2i.json (API format) to ComfyUI over its HTTP API,
one prompt at a time: each prompt gets a fresh random seed, and the next one is submitted only
when the previous one has finished (one pending prompt, IMG-091). It stops submitting once
--seconds have elapsed and waits for the running prompt to finish. It never touches prompts it
did not submit.

Before each submit it touches the --trim-flag file (IMG-040: the canvas trims its GPU tile cache
to the visible pages when that file's mtime changes).

Exit codes: 0 = ran, 1 = a prompt was rejected or failed, 2 = ComfyUI unreachable.

Only the Python standard library is used.

  python spikes/canvas/load/comfy_load.py --seconds 120 --trim-flag %TEMP%\\ww-spike-trim
"""

import argparse
import json
import os
import random
import struct
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_WORKFLOW = os.path.normpath(os.path.join(HERE, "..", "..", "comfy-measure", "workflows", "zimage_t2i.json"))


def now_ms():
    return int(time.time() * 1000)


class Comfy:
    def __init__(self, host, port):
        self.base = f"http://{host}:{port}"

    def request(self, method, path, body=None, timeout=10.0):
        data = None
        headers = {}
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(self.base + path, data=data, method=method, headers=headers)
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
            ctype = r.headers.get("Content-Type", "")
        if "json" in ctype:
            return json.loads(raw.decode("utf-8")) if raw else None
        return raw

    def get_json(self, path, timeout=10.0):
        return self.request("GET", path, timeout=timeout)

    def post_json(self, path, body, timeout=10.0):
        return self.request("POST", path, body, timeout=timeout)


def reachable(comfy, timeout):
    try:
        stats = comfy.get_json("/system_stats", timeout=timeout)
        return isinstance(stats, dict), stats
    except (urllib.error.URLError, OSError, ValueError):
        return False, None


def touch(path):
    with open(path, "a", encoding="utf-8"):
        pass
    t = time.time()
    os.utime(path, (t, t))
    return int(t * 1000)


def randomize_seeds(prompt, rng):
    """Sets every integer 'seed' / 'noise_seed' input to a fresh random value; returns them."""
    seeds = {}
    for node_id, node in prompt.items():
        inputs = node.get("inputs", {})
        for key in ("seed", "noise_seed"):
            if key in inputs and isinstance(inputs[key], int):
                inputs[key] = rng.randrange(0, 2**53)
                seeds[f"{node_id}.{key}"] = inputs[key]
    return seeds


def to_preview(prompt):
    """SaveImage -> PreviewImage: same GPU work, but the images go to ComfyUI's temp folder
    (cleared on restart) instead of filling its output folder."""
    changed = []
    for node_id, node in prompt.items():
        if node.get("class_type") == "SaveImage":
            node["class_type"] = "PreviewImage"
            node["inputs"].pop("filename_prefix", None)
            changed.append(node_id)
    return changed


def png_size(data):
    if len(data) >= 24 and data[:8] == b"\x89PNG\r\n\x1a\n" and data[12:16] == b"IHDR":
        return struct.unpack(">II", data[16:24])
    return None


def wait_for(comfy, prompt_id, poll, timeout):
    """Polls /history/<id> until the prompt has finished; returns its history entry or None."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            h = comfy.get_json(f"/history/{prompt_id}", timeout=10.0)
        except (urllib.error.URLError, OSError, ValueError):
            h = None
        if isinstance(h, dict) and prompt_id in h:
            entry = h[prompt_id]
            status = entry.get("status") or {}
            if status.get("completed") or status.get("status_str") in ("success", "error"):
                return entry
        time.sleep(poll)
    return None


def output_sizes(comfy, entry):
    sizes = []
    for node_out in (entry.get("outputs") or {}).values():
        for img in node_out.get("images", []) or []:
            q = urllib.parse.urlencode({"filename": img.get("filename", ""), "subfolder": img.get("subfolder", ""),
                                        "type": img.get("type", "output")})
            try:
                data = comfy.request("GET", f"/view?{q}", timeout=30.0)
                sizes.append(png_size(data) if isinstance(data, (bytes, bytearray)) else None)
            except (urllib.error.URLError, OSError):
                sizes.append(None)
    return sizes


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8188)
    ap.add_argument("--seconds", type=float, default=120.0, help="stop submitting after this many seconds")
    ap.add_argument("--trim-flag", help="file to touch before each submit")
    ap.add_argument("--workflow", default=DEFAULT_WORKFLOW, help="API-format workflow JSON")
    ap.add_argument("--unload-first", action="store_true", help="POST /free (unload models) before the first prompt")
    ap.add_argument("--save-outputs", action="store_true", help="keep SaveImage (default: PreviewImage, temp folder)")
    ap.add_argument("--check-sizes", action="store_true", help="download every output and record its PNG size")
    ap.add_argument("--max-prompts", type=int, default=0, help="stop after N prompts (0 = no limit)")
    ap.add_argument("--summary", help="write a JSON summary here")
    ap.add_argument("--poll", type=float, default=0.1, help="history poll interval (s)")
    ap.add_argument("--prompt-timeout", type=float, default=900.0)
    args = ap.parse_args()

    t_start = time.monotonic()
    comfy = Comfy(args.host, args.port)
    ok, stats = reachable(comfy, timeout=2.0)
    if not ok:
        print(f"ComfyUI unreachable at {args.host}:{args.port}", flush=True)
        return 2

    if not os.path.isfile(args.workflow):
        print(f"workflow not found: {args.workflow}", file=sys.stderr, flush=True)
        return 1
    with open(args.workflow, encoding="utf-8") as f:
        base = json.load(f)
    if not isinstance(base, dict) or not all(isinstance(v, dict) and "class_type" in v for v in base.values()):
        print(f"not an API-format workflow: {args.workflow}", file=sys.stderr, flush=True)
        return 1

    summary = {
        "started_epoch_ms": now_ms(),
        "comfy": f"{args.host}:{args.port}",
        "workflow": args.workflow,
        "seconds": args.seconds,
        "trim_flag": args.trim_flag,
        "comfyui_version": ((stats or {}).get("system") or {}).get("comfyui_version"),
        "pytorch_version": ((stats or {}).get("system") or {}).get("pytorch_version"),
        "devices": [{"name": d.get("name"), "vram_total": d.get("vram_total")} for d in (stats or {}).get("devices", [])],
        "save_outputs": args.save_outputs,
        "prompts": [],
    }
    client_id = str(uuid.uuid4())
    rng = random.Random()
    rc = 0

    if args.unload_first:
        try:
            comfy.post_json("/free", {"unload_models": True, "free_memory": True})
            summary["free"] = {"epoch_ms": now_ms(), "ok": True}
        except (urllib.error.URLError, OSError) as e:
            summary["free"] = {"epoch_ms": now_ms(), "ok": False, "error": str(e)}
        time.sleep(2.0)  # /free is applied by the prompt worker between prompts

    i = 0
    while time.monotonic() - t_start < args.seconds:
        if args.max_prompts and i >= args.max_prompts:
            break
        prompt = json.loads(json.dumps(base))
        seeds = randomize_seeds(prompt, rng)
        preview_nodes = [] if args.save_outputs else to_preview(prompt)
        rec = {"i": i, "seeds": seeds, "preview_nodes": preview_nodes}
        if args.trim_flag:
            rec["trim_touch_epoch_ms"] = touch(args.trim_flag)
        rec["submitted_epoch_ms"] = now_ms()
        try:
            resp = comfy.post_json("/prompt", {"prompt": prompt, "client_id": client_id}, timeout=30.0)
        except urllib.error.HTTPError as e:
            body = e.read().decode("utf-8", "replace")
            rec.update({"status": "rejected", "http": e.code, "error": body[:4000]})
            summary["prompts"].append(rec)
            print(f"prompt {i} rejected: HTTP {e.code}: {body[:400]}", file=sys.stderr, flush=True)
            rc = 1
            break
        except (urllib.error.URLError, OSError) as e:
            rec.update({"status": "unreachable", "error": str(e)})
            summary["prompts"].append(rec)
            print(f"ComfyUI unreachable at {args.host}:{args.port}", flush=True)
            rc = 2
            break
        node_errors = (resp or {}).get("node_errors") or {}
        rec["prompt_id"] = (resp or {}).get("prompt_id")
        rec["node_errors"] = node_errors
        if node_errors or not rec["prompt_id"]:
            rec["status"] = "rejected"
            summary["prompts"].append(rec)
            print(f"prompt {i} rejected: node_errors={json.dumps(node_errors)[:400]}", file=sys.stderr, flush=True)
            rc = 1
            break
        entry = wait_for(comfy, rec["prompt_id"], args.poll, args.prompt_timeout)
        rec["finished_epoch_ms"] = now_ms()
        rec["duration_ms"] = rec["finished_epoch_ms"] - rec["submitted_epoch_ms"]
        if entry is None:
            rec["status"] = "timeout"
            rc = 1
        else:
            st = entry.get("status") or {}
            rec["status"] = st.get("status_str") or ("success" if st.get("completed") else "unknown")
            msgs = st.get("messages") or []
            for m in msgs:
                if isinstance(m, list) and len(m) == 2 and m[0] in ("execution_start", "execution_success", "execution_error"):
                    rec[m[0] + "_ms"] = (m[1] or {}).get("timestamp")
            if rec["status"] != "success":
                rec["error"] = json.dumps(msgs)[-4000:]
                rc = 1
            if args.check_sizes or i == 0:
                rec["output_sizes"] = output_sizes(comfy, entry)
        summary["prompts"].append(rec)
        print(f"prompt {i}: {rec['status']} in {rec['duration_ms']} ms", flush=True)
        i += 1
        if rc:
            break

    summary["finished_epoch_ms"] = now_ms()
    done = [p for p in summary["prompts"] if p.get("status") == "success"]
    summary["prompts_ok"] = len(done)
    summary["prompts_total"] = len(summary["prompts"])
    durs = sorted(p["duration_ms"] for p in done)
    if durs:
        summary["duration_ms_p50"] = durs[len(durs) // 2]
        summary["duration_ms_max"] = durs[-1]
    if args.summary:
        with open(args.summary, "w", encoding="utf-8") as f:
            json.dump(summary, f, indent=1)
    return rc


if __name__ == "__main__":
    sys.exit(main())
