#!/usr/bin/env python3
"""Keep ComfyUI's GPU busy for S0.6 (spike 10): a prompt is always queued behind the running one.

spikes/canvas/load/comfy_load.py submits one prompt at a time and waits for it, so the GPU idles
between prompts (the owner's first S0.6 run: only about 75 % of 500 ms samples at >= 95 %). This
loader keeps --depth prompts (default 2) outstanding, so the next prompt is already queued when
one finishes. Each prompt gets fresh random seeds, and SaveImage becomes PreviewImage (images go
to ComfyUI's temp folder). It runs until --seconds have passed, the --stop-file exists, or the
--parent-pid process is gone (so closing comfy-load.ps1's window doesn't leave the GPU loaded);
then it stops submitting and exits without waiting. comfy-load.ps1 stops ComfyUI afterwards. It
only touches prompts it submitted. The summary is written on every exit path.

Only the Python standard library is used. Exit codes: 0 = ran, 1 = a prompt was rejected or
failed, 2 = ComfyUI unreachable.

  python -I spikes/app-shell/comfy/keep_busy.py --workflow spikes/app-shell/comfy/load-workflow.json
"""

import argparse
import ctypes
import json
import os
import random
import sys
import time
import urllib.error
import urllib.request
import uuid


def now_ms():
    return int(time.time() * 1000)


def to_submit(outstanding, depth):
    """How many prompts to submit now so that `depth` are outstanding (never negative)."""
    return max(0, depth - outstanding)


def finished(history_entry):
    """True when a /history entry says the prompt is done (success or error)."""
    if not isinstance(history_entry, dict):
        return False
    st = history_entry.get("status") or {}
    return bool(st.get("completed")) or st.get("status_str") in ("success", "error")


def prepare(base, rng):
    """A copy of the workflow with fresh seeds and SaveImage swapped for PreviewImage."""
    prompt = json.loads(json.dumps(base))
    for node in prompt.values():
        inputs = node.get("inputs", {})
        for key in ("seed", "noise_seed"):
            if isinstance(inputs.get(key), int):
                inputs[key] = rng.randrange(0, 2**53)
        if node.get("class_type") == "SaveImage":
            node["class_type"] = "PreviewImage"
            inputs.pop("filename_prefix", None)
    return prompt


def process_alive(pid):
    """Windows: is the process still running (OpenProcess + GetExitCodeProcess == STILL_ACTIVE)?"""
    if not pid:
        return True
    if os.name != "nt":
        try:
            os.kill(pid, 0)
            return True
        except OSError:
            return False
    k32 = ctypes.windll.kernel32
    h = k32.OpenProcess(0x1000, False, int(pid))  # PROCESS_QUERY_LIMITED_INFORMATION
    if not h:
        return False
    code = ctypes.c_ulong()
    ok = k32.GetExitCodeProcess(h, ctypes.byref(code))
    k32.CloseHandle(h)
    return bool(ok) and code.value == 259  # STILL_ACTIVE


class Comfy:
    def __init__(self, host, port):
        self.base = f"http://{host}:{port}"

    def call(self, method, path, body=None, timeout=10.0):
        data = json.dumps(body).encode("utf-8") if body is not None else None
        req = urllib.request.Request(self.base + path, data=data, method=method,
                                     headers={"Content-Type": "application/json"} if data else {})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
        return json.loads(raw.decode("utf-8")) if raw else None


def run(comfy, base, depth, should_stop, client_id="keep-busy", rng=None, poll=0.1, sleep=time.sleep, log=None):
    """The loop: keep `depth` prompts outstanding until should_stop(); returns the exit code.

    `comfy` needs call(method, path, body=None, timeout=...); `log` (a list) gets one record per
    submitted prompt. Errors that mean "ComfyUI isn't answering properly" (HTTP, network, bad
    JSON) end the loop with code 2; a rejected or failed prompt ends it with code 1.
    """
    rng = rng or random.Random()
    log = log if log is not None else []
    outstanding = []  # prompt ids, oldest first
    while not should_stop():
        for _ in range(to_submit(len(outstanding), depth)):
            try:
                resp = comfy.call("POST", "/prompt", {"prompt": prepare(base, rng), "client_id": client_id}, timeout=30.0)
            except urllib.error.HTTPError as e:
                print(f"prompt rejected: HTTP {e.code}", file=sys.stderr, flush=True)
                return 1
            except (urllib.error.URLError, OSError, ValueError) as e:
                print(f"ComfyUI not answering properly: {e}", file=sys.stderr, flush=True)
                return 2
            if not isinstance(resp, dict) or resp.get("node_errors") or not resp.get("prompt_id"):
                print(f"prompt rejected: {json.dumps(resp)[:400]}", file=sys.stderr, flush=True)
                return 1
            outstanding.append(resp["prompt_id"])
            log.append({"id": resp["prompt_id"], "submitted_epoch_ms": now_ms()})
        try:
            h = comfy.call("GET", f"/history/{outstanding[0]}", timeout=10.0)
        except (urllib.error.URLError, OSError, ValueError):
            h = None
        entry = h.get(outstanding[0]) if isinstance(h, dict) else None
        if finished(entry):
            pid = outstanding.pop(0)
            status = (entry.get("status") or {}).get("status_str") or "success"
            for rec in log:
                if rec["id"] == pid:
                    rec.update({"finished_epoch_ms": now_ms(), "status": status})
            print(f"prompt {pid[:8]}: {status}", flush=True)
            if status != "success":
                return 1
        else:
            sleep(poll)
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8188)
    ap.add_argument("--workflow", required=True)
    ap.add_argument("--depth", type=int, default=2, help="prompts kept outstanding")
    ap.add_argument("--seconds", type=float, default=1800.0)
    ap.add_argument("--stop-file", help="stop when this file exists")
    ap.add_argument("--parent-pid", type=int, default=0, help="stop when this process is gone")
    ap.add_argument("--summary", help="write a JSON summary here")
    ap.add_argument("--poll", type=float, default=0.1)
    args = ap.parse_args()

    log = []
    rc = 2
    try:
        comfy = Comfy(args.host, args.port)
        try:
            comfy.call("GET", "/system_stats", timeout=3.0)
        except (urllib.error.URLError, OSError, ValueError):
            print(f"ComfyUI unreachable at {args.host}:{args.port}", flush=True)
            return 2
        with open(args.workflow, encoding="utf-8") as f:
            base = json.load(f)
        t0 = time.monotonic()

        def should_stop():
            return (time.monotonic() - t0 >= args.seconds
                    or (args.stop_file and os.path.exists(args.stop_file))
                    or not process_alive(args.parent_pid))

        rc = run(comfy, base, args.depth, should_stop, client_id=str(uuid.uuid4()), poll=args.poll, log=log)
        return rc
    finally:
        if args.summary:
            with open(args.summary, "w", encoding="utf-8") as f:
                json.dump({"depth": args.depth, "prompts": log, "rc": rc}, f, indent=1)


if __name__ == "__main__":
    sys.exit(main())
