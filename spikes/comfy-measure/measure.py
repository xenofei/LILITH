#!/usr/bin/env python3
"""ComfyUI measurement driver for the M0 Task 8 spike (IMG-155, IMG-157, IMG-158, IMG-160).

Standard library only. Talks to ComfyUI over HTTP and its websocket on 127.0.0.1:8188 (loopback only).

Every run:
  * submits exactly one prompt and waits for it to finish (websocket execution_success/error);
  * sets extra_data.preview_method (IMG-045);
  * samples every 100 ms:
      - total dedicated VRAM: `nvidia-smi --query-gpu=memory.used -lms 100` (MiB);
      - per-process dedicated VRAM: the PDH counter
        \\GPU Process Memory(pid_<pid>_*)\\Dedicated Usage, summed over the process's instances,
        for ComfyUI and for every --watch-proc process. The counter is read through pdh.dll
        (ctypes), because typeperf's smallest sample interval is 1 s;
  * records the wall time from execution_start to the last `executed` message, plus per-node times;
  * collects ComfyUI log lines (from /internal/logs/raw) that match
    `out of memory|OOM|lowvram|offload|partially loaded`;
  * writes one JSON per run to results/.

Placeholders filled in the workflow (API format, workflows/<name>.json):
  {{MODEL}}  -> the diffusion model file for --model (bf16 | int8)
  {{LORA}}   -> --lora file name; without --lora the LoraLoaderModelOnly node is spliced out
  {{IMAGE}}  -> --image is uploaded to ComfyUI's temp folder under a run-unique name before each run
               (outside the timed window), so every run re-executes from LoadImage like a fresh canvas
               export would
  {{MASK}}   -> --mask, uploaded the same way
  zimage_t2i.json (Task 4's export, kept byte-identical for comfy_load.py and Task 6) has no placeholder;
  there a UNETLoader whose unet_name is one of the Z-Image files is the {{MODEL}} slot.
  {{PROMPT}} -> --text (SAM text prompt)
Seeds: every KSampler seed is set to --seed + run index (0-based), so different configurations get the
same seed sequence and every run does real sampling work (identical prompts would be served from cache).
SaveImage nodes are turned into PreviewImage (outputs go to ComfyUI's temp folder, not output/).

Exit codes: 0 ok, 1 a run failed, 2 ComfyUI unreachable.
"""
import argparse
import base64
import ctypes
import json
import os
import re
import socket
import struct
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from ctypes import wintypes
from datetime import datetime
from pathlib import Path

HOST = "127.0.0.1"
PORT = 8188
HERE = Path(__file__).resolve().parent
WORKFLOWS = HERE / "workflows"
RESULTS = HERE / "results"

LUID = "luid_"  # the NVIDIA adapter's PDH instance prefix, detected in main()


def detect_luid():
    """PDH adapter instance (luid_..._phys_N) whose dedicated usage is closest to nvidia-smi memory.used."""
    smi = subprocess.run(["nvidia-smi", "--query-gpu=memory.used", "--format=csv,noheader,nounits", "-i", "0"],
                         capture_output=True, text=True).stdout.strip()
    pdh = ctypes.WinDLL("pdh.dll")
    q = ctypes.c_void_p()
    pdh.PdhOpenQueryW(None, None, ctypes.byref(q))
    h = ctypes.c_void_p()
    pdh.PdhAddEnglishCounterW(q, "\\GPU Adapter Memory(*)\\Dedicated Usage", None, ctypes.byref(h))
    pdh.PdhCollectQueryData(q)
    items = PdhSampler({})._items(pdh, h)
    pdh.PdhCloseQuery(q)
    best = min(items, key=lambda it: abs(it[1] / 1048576.0 - float(smi)))
    return best[0].rsplit("_phys", 1)[0], {n: round(v / 1048576.0) for n, v in items}, int(smi)


EXTRA_KINDS = ("Local", "Shared", "Total Committed")

MODELS = {
    "bf16": "z_image_turbo_bf16.safetensors",
    "int8": "z_image_turbo_int8_convrot.safetensors",
}
OOM_PATTERN = re.compile(r"out of memory|OOM|lowvram|offload|partially loaded")
# Lines that match OOM_PATTERN only because they describe configuration, not an event:
#   "CLIP/text encoder model load device: cuda:0, offload device: cpu, ..."  (comfy/sd.py, every TE load)
#   "VAE load device: cuda:0, offload device: cpu, dtype: ..."               (comfy/sd.py, every VAE load)
#   "Using async weight offloading with N streams"                            (startup)
CONFIG_ONLY = re.compile(r"load device: .*, offload device: |Using async weight offloading")


def unreachable():
    print(f"ComfyUI unreachable at {HOST}:{PORT}")
    sys.exit(2)


# ----------------------------------------------------------------------------------------------- HTTP
def http(method, path, body=None, timeout=30, raw=False):
    url = f"http://{HOST}:{PORT}{path}"
    data = None
    headers = {}
    if body is not None:
        if isinstance(body, (bytes, bytearray)):
            data = bytes(body)
        else:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    with urllib.request.urlopen(req, timeout=timeout) as r:
        payload = r.read()
    if raw:
        return payload
    return json.loads(payload) if payload else None


def upload_temp(path, name):
    """POST /upload/image (multipart) into ComfyUI's temp folder. Returns the annotated name."""
    ctype = "image/jpeg" if str(path).lower().endswith((".jpg", ".jpeg")) else "image/png"
    boundary = uuid.uuid4().hex
    content = Path(path).read_bytes()
    parts = []
    def field(n, v):
        parts.append(f"--{boundary}\r\nContent-Disposition: form-data; name=\"{n}\"\r\n\r\n{v}\r\n".encode())
    field("type", "temp")
    field("overwrite", "true")
    parts.append((f"--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{name}\"\r\n"
                  f"Content-Type: {ctype}\r\n\r\n").encode() + content + b"\r\n")
    parts.append(f"--{boundary}--\r\n".encode())
    req = urllib.request.Request(f"http://{HOST}:{PORT}/upload/image", data=b"".join(parts), method="POST",
                                 headers={"Content-Type": f"multipart/form-data; boundary={boundary}"})
    with urllib.request.urlopen(req, timeout=30) as r:
        info = json.loads(r.read())
    sub = info.get("subfolder") or ""
    rel = f"{sub}/{info['name']}" if sub else info["name"]
    return f"{rel} [temp]"


# ------------------------------------------------------------------------------------------ websocket
class WebSocket:
    """Minimal RFC 6455 client (text + binary frames, ping/pong, fragmentation)."""

    def __init__(self, path):
        self.sock = socket.create_connection((HOST, PORT), timeout=10)
        key = base64.b64encode(os.urandom(16)).decode()
        req = (f"GET {path} HTTP/1.1\r\nHost: {HOST}:{PORT}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
               f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n")
        self.sock.sendall(req.encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("websocket handshake: connection closed")
            buf += chunk
        head, self.buf = buf.split(b"\r\n\r\n", 1)
        if b" 101 " not in head.split(b"\r\n", 1)[0]:
            raise ConnectionError("websocket handshake failed: " + head.split(b"\r\n", 1)[0].decode(errors="replace"))

    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(max(65536, n - len(self.buf)))
            if not chunk:
                raise ConnectionError("websocket closed")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def _send(self, opcode, payload=b""):
        hdr = bytearray([0x80 | opcode])
        n = len(payload)
        if n < 126:
            hdr.append(0x80 | n)
        elif n < 65536:
            hdr.append(0x80 | 126)
            hdr += struct.pack(">H", n)
        else:
            hdr.append(0x80 | 127)
            hdr += struct.pack(">Q", n)
        mask = os.urandom(4)
        hdr += mask
        self.sock.sendall(bytes(hdr) + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))

    def recv(self, timeout):
        """Returns ('text', str) or ('binary', bytes); raises socket.timeout."""
        self.sock.settimeout(timeout)
        message, kind = b"", None
        while True:
            b0, b1 = self._read(2)
            fin, opcode = b0 & 0x80, b0 & 0x0F
            n = b1 & 0x7F
            if n == 126:
                n = struct.unpack(">H", self._read(2))[0]
            elif n == 127:
                n = struct.unpack(">Q", self._read(8))[0]
            if b1 & 0x80:
                mask = self._read(4)
                payload = bytes(b ^ mask[i % 4] for i, b in enumerate(self._read(n)))
            else:
                payload = self._read(n)
            if opcode == 0x9:
                self._send(0xA, payload)
                continue
            if opcode == 0xA:
                continue
            if opcode == 0x8:
                raise ConnectionError("websocket closed by server")
            if opcode in (0x1, 0x2):
                kind = "text" if opcode == 0x1 else "binary"
            message += payload
            if fin:
                return (kind, message.decode() if kind == "text" else message)

    def close(self):
        try:
            self._send(0x8, b"")
        except OSError:
            pass
        self.sock.close()


# ------------------------------------------------------------------------------------------- sampling
class PdhSampler(threading.Thread):
    """Per-process dedicated VRAM via PDH (pdh.dll), every `interval` s, summed per pid."""

    PDH_FMT_LARGE = 0x400
    ERROR_SUCCESS = 0
    PDH_MORE_DATA = 0x800007D2

    class FMT_VALUE(ctypes.Structure):
        _fields_ = [("CStatus", wintypes.DWORD), ("pad", wintypes.DWORD), ("largeValue", ctypes.c_longlong)]

    class FMT_ITEM(ctypes.Structure):
        pass

    FMT_ITEM._fields_ = [("szName", wintypes.LPWSTR), ("FmtValue", FMT_VALUE)]

    def __init__(self, pids, interval=0.1):
        super().__init__(daemon=True)
        self.pids = dict(pids)  # label -> [pid, ...]
        self.interval = interval
        self.samples = []  # (t, {label: MiB})  Dedicated Usage (the counter the brief names)
        # (t, {label: MiB}) per extra counter: Local Usage (resident in VRAM), Shared Usage / Non Local
        # (system memory the GPU maps for the process), Total Committed (allocated, resident or not)
        self.extra = {k: [] for k in EXTRA_KINDS}
        self.adapter = []  # (t, MiB) total over all processes (\GPU Adapter Memory(*)\Dedicated Usage)
        self.stop_evt = threading.Event()
        self.error = None

    def run(self):
        try:
            pdh = ctypes.WinDLL("pdh.dll")
            q = ctypes.c_void_p()
            if pdh.PdhOpenQueryW(None, None, ctypes.byref(q)) != 0:
                raise OSError("PdhOpenQueryW failed")
            counters = []  # (kind, label, handle)
            for label, pids in self.pids.items():
                for pid in pids:
                    for kind in ("Dedicated",) + EXTRA_KINDS:
                        h = ctypes.c_void_p()
                        name = kind if kind == "Total Committed" else f"{kind} Usage"
                        path = f"\\GPU Process Memory(pid_{pid}_{LUID}*)\\{name}"
                        if pdh.PdhAddEnglishCounterW(q, path, None, ctypes.byref(h)) == 0:
                            counters.append((kind, label, h))
            ah = ctypes.c_void_p()
            pdh.PdhAddEnglishCounterW(q, f"\\GPU Adapter Memory({LUID}*)\\Dedicated Usage", None, ctypes.byref(ah))
            t0 = time.perf_counter()
            while not self.stop_evt.is_set():
                t = time.perf_counter()
                pdh.PdhCollectQueryData(q)
                rows = {k: {label: 0.0 for label in self.pids} for k in ("Dedicated",) + EXTRA_KINDS}
                for kind, label, h in counters:
                    rows[kind][label] += self._sum(pdh, h) / 1048576.0
                now = time.time()
                self.samples.append((now, rows["Dedicated"]))
                for k in EXTRA_KINDS:
                    self.extra[k].append((now, rows[k]))
                self.adapter.append((time.time(), self._max(pdh, ah) / 1048576.0))
                self.stop_evt.wait(max(0.0, self.interval - (time.perf_counter() - t)))
            pdh.PdhCloseQuery(q)
        except Exception as e:  # noqa: BLE001 - recorded in the result
            self.error = repr(e)

    def _items(self, pdh, h):
        size = wintypes.DWORD(0)
        count = wintypes.DWORD(0)
        rc = pdh.PdhGetFormattedCounterArrayW(h, self.PDH_FMT_LARGE, ctypes.byref(size), ctypes.byref(count), None)
        if (rc & 0xFFFFFFFF) != self.PDH_MORE_DATA or size.value == 0:
            return []
        buf = ctypes.create_string_buffer(size.value)
        rc = pdh.PdhGetFormattedCounterArrayW(h, self.PDH_FMT_LARGE, ctypes.byref(size), ctypes.byref(count), buf)
        if rc != 0:
            return []
        arr = ctypes.cast(buf, ctypes.POINTER(self.FMT_ITEM))
        return [(arr[i].szName, arr[i].FmtValue.largeValue) for i in range(count.value) if arr[i].FmtValue.CStatus in (0, 1)]

    def _sum(self, pdh, h):
        return float(sum(v for _, v in self._items(pdh, h)))

    def _max(self, pdh, h):
        items = self._items(pdh, h)
        return float(max((v for _, v in items), default=0))

    def stop(self):
        self.stop_evt.set()
        self.join(timeout=5)


class SmiSampler(threading.Thread):
    """Total dedicated VRAM in use on GPU 0 (nvidia-smi memory.used, MiB) every 100 ms."""

    def __init__(self):
        super().__init__(daemon=True)
        self.samples = []
        self.proc = None
        self.error = None

    def run(self):
        try:
            self.proc = subprocess.Popen(
                ["nvidia-smi", "--query-gpu=memory.used", "--format=csv,noheader,nounits", "-i", "0", "-lms", "100"],
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, creationflags=0x08000000)
            for line in self.proc.stdout:
                line = line.strip()
                if line.isdigit():
                    self.samples.append((time.time(), int(line)))
        except Exception as e:  # noqa: BLE001
            self.error = repr(e)

    def stop(self):
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.join(timeout=5)


def listening_pid(port):
    out = subprocess.run(["netstat", "-ano", "-p", "TCP"], capture_output=True, text=True).stdout
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 5 and parts[3] == "LISTENING" and parts[1].endswith(f":{port}"):
            return int(parts[4])
    return None


def vram_snapshot(top=12):
    """Dedicated VRAM per process right now (PDH, all processes), largest first, with image names."""
    try:
        pdh = ctypes.WinDLL("pdh.dll")
        q = ctypes.c_void_p()
        pdh.PdhOpenQueryW(None, None, ctypes.byref(q))
        h = ctypes.c_void_p()
        pdh.PdhAddEnglishCounterW(q, "\\GPU Process Memory(*)\\Dedicated Usage", None, ctypes.byref(h))
        pdh.PdhCollectQueryData(q)
        per = {}
        for name, v in PdhSampler({})._items(pdh, h):
            m = re.match(r"pid_(\d+)_", name or "")
            if LUID not in (name or ""):
                continue
            if m:
                per[int(m.group(1))] = per.get(int(m.group(1)), 0) + v
        pdh.PdhCloseQuery(q)
        names = {}
        res = subprocess.run(["tasklist", "/fo", "csv", "/nh"], capture_output=True, text=True).stdout
        for line in res.splitlines():
            cols = [c.strip('"') for c in line.split('","')]
            if len(cols) >= 2 and cols[1].isdigit():
                names[int(cols[1])] = cols[0]
        rows = sorted(per.items(), key=lambda kv: -kv[1])[:top]
        return [{"pid": pid, "name": names.get(pid, "?"), "dedicated_mib": round(v / 1048576.0, 1)} for pid, v in rows]
    except Exception as e:  # noqa: BLE001
        return [{"error": repr(e)}]


def pids_by_name(names):
    out = {}
    if not names:
        return out
    res = subprocess.run(["tasklist", "/fo", "csv", "/nh"], capture_output=True, text=True).stdout
    for line in res.splitlines():
        cols = [c.strip('"') for c in line.split('","')]
        if len(cols) >= 2 and cols[0].lower() in names:
            out.setdefault(cols[0], []).append(int(cols[1]))
    return out


# ---------------------------------------------------------------------------------------- statistics
def series(values):
    v = sorted(values)
    if not v:
        return {"n": 0}
    def pct(p):
        k = (len(v) - 1) * p
        f = int(k)
        c = min(f + 1, len(v) - 1)
        return v[f] + (v[c] - v[f]) * (k - f)
    return {"n": len(v), "p50": pct(0.5), "p95": pct(0.95), "p99": pct(0.99), "max": v[-1], "min": v[0]}


def jpeg_size(data):
    """(width, height) from a JPEG's SOF marker, or PNG IHDR."""
    if data[:8] == b"\x89PNG\r\n\x1a\n":
        return struct.unpack(">II", data[16:24])
    i = 2
    while i + 9 < len(data):
        if data[i] != 0xFF:
            i += 1
            continue
        marker = data[i + 1]
        if marker in (0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF):
            h, w = struct.unpack(">HH", data[i + 5:i + 9])
            return (w, h)
        seg = struct.unpack(">H", data[i + 2:i + 4])[0]
        i += 2 + seg
    return None


# ------------------------------------------------------------------------------------------ workflow
def splice_out(graph, node_id):
    """Remove node_id and rewire every consumer of its output 0 to the node's own `model` input."""
    src = graph[node_id]["inputs"]["model"]
    for nid, node in graph.items():
        for k, v in node["inputs"].items():
            if isinstance(v, list) and len(v) == 2 and v[0] == node_id:
                node["inputs"][k] = list(src)
    del graph[node_id]


def prepare(template, args, run_index, image_name, mask_name):
    g = json.loads(json.dumps(template))
    for nid in list(g):
        node = g[nid]
        ins = node["inputs"]
        if ins.get("lora_name") == "{{LORA}}":
            if args.lora:
                ins["lora_name"] = args.lora
                if "strength_model" in ins:
                    ins["strength_model"] = args.lora_strength
            else:
                splice_out(g, nid)
                continue
        for k, v in list(ins.items()):
            if v == "{{MODEL}}":
                ins[k] = MODELS[args.model]
            elif v == "{{IMAGE}}":
                ins[k] = image_name
            elif v == "{{MASK}}":
                ins[k] = mask_name
            elif v == "{{PROMPT}}":
                ins[k] = args.text
            elif isinstance(v, str) and "{{PROMPT}}" in v:
                ins[k] = v.replace("{{PROMPT}}", args.text)
        if node["class_type"] == "UNETLoader" and args.model and ins.get("unet_name") in MODELS.values():
            ins["unet_name"] = MODELS[args.model]
        if node["class_type"] in ("KSampler",) and "seed" in ins:
            ins["seed"] = args.seed + run_index
        if node["class_type"] == "SaveImage":
            node["class_type"] = "PreviewImage"
            ins.pop("filename_prefix", None)
    leftovers = [k for k, v in g.items() for x in v["inputs"].values() if isinstance(x, str) and "{{" in x]
    if leftovers:
        raise SystemExit(f"unfilled placeholders in nodes {sorted(set(leftovers))} (missing --model/--image/--text?)")
    return g


def comfy_logs():
    try:
        return http("GET", "/internal/logs/raw", timeout=10).get("entries", [])
    except Exception:  # noqa: BLE001
        return []


# ------------------------------------------------------------------------------------------------ run
def one_run(args, template, run_index, client_id, pdh_targets, out_dir, stamp):
    image_name = mask_name = None
    if args.image:
        image_name = upload_temp(args.image, f"ww-spike-{stamp}-{run_index:02d}{Path(args.image).suffix}")
    if args.mask:
        mask_name = upload_temp(args.mask, f"ww-spike-{stamp}-{run_index:02d}-mask{Path(args.mask).suffix}")
    graph = prepare(template, args, run_index, image_name, mask_name)

    snapshot = vram_snapshot()
    ws = WebSocket(f"/ws?clientId={client_id}")
    smi = SmiSampler()
    pdh = PdhSampler(pdh_targets)
    smi.start()
    pdh.start()
    time.sleep(0.5)  # baseline samples before submit
    log_mark = datetime.now().isoformat()
    t_submit = time.time()
    resp = http("POST", "/prompt", {"prompt": graph, "client_id": client_id,
                                     "extra_data": {"preview_method": args.preview}})
    prompt_id = resp.get("prompt_id")
    node_errors = resp.get("node_errors") or {}
    events = []
    previews = []
    t_start = t_last_executed = t_end = None
    status = "unknown"
    error = None
    cur_node, cur_t = None, None
    node_ms = {}
    outputs = {}
    deadline = time.time() + args.timeout
    try:
        while time.time() < deadline:
            try:
                kind, msg = ws.recv(timeout=max(0.1, deadline - time.time()))
            except socket.timeout:
                break
            now = time.time()
            if kind == "binary":
                etype = struct.unpack(">I", msg[:4])[0]
                if etype == 1:  # PREVIEW_IMAGE: 4-byte image type then the image
                    img = msg[8:]
                    size = jpeg_size(img)
                    previews.append({"t": now, "bytes": len(img), "size": list(size) if size else None,
                                     "format": {1: "JPEG", 2: "PNG"}.get(struct.unpack(">I", msg[4:8])[0])})
                    if args.save_previews:
                        out_dir.mkdir(parents=True, exist_ok=True)
                        (out_dir / f"preview-{len(previews):02d}.jpg").write_bytes(img)
                continue
            m = json.loads(msg)
            typ, data = m.get("type"), m.get("data") or {}
            if data.get("prompt_id") not in (None, prompt_id):
                continue
            if typ not in ("progress", "progress_state", "status", "crystools.monitor"):
                events.append({"t": now, "type": typ, "node": data.get("node")})
            if typ == "execution_start":
                t_start = now
            elif typ == "executing":
                if cur_node is not None:
                    node_ms[cur_node] = node_ms.get(cur_node, 0) + (now - cur_t) * 1000
                cur_node, cur_t = data.get("node"), now
                if data.get("node") is None:
                    t_end = now
            elif typ == "executed":
                t_last_executed = now
                outputs[data.get("node")] = data.get("output")
            elif typ == "execution_cached":
                events[-1]["cached"] = data.get("nodes")
            elif typ == "execution_success":
                if cur_node is not None:
                    node_ms[cur_node] = node_ms.get(cur_node, 0) + (now - cur_t) * 1000
                    cur_node = None
                t_end = t_end or now
                status = "success"
                break
            elif typ in ("execution_error", "execution_interrupted"):
                status = typ
                error = {k: data.get(k) for k in ("node_id", "node_type", "exception_message", "exception_type")}
                t_end = now
                break
        else:
            status = "timeout"
        if status == "unknown":
            status = "timeout"
    finally:
        time.sleep(0.5)  # trailing samples
        smi.stop()
        pdh.stop()
        ws.close()

    # outputs: fetch the first image of every output node (PreviewImage -> temp)
    saved = []
    if args.save_outputs and status == "success":
        out_dir.mkdir(parents=True, exist_ok=True)
        for nid, out in outputs.items():
            for img in (out or {}).get("images", [])[:1]:
                q = urllib.parse.urlencode({"filename": img["filename"], "subfolder": img.get("subfolder", ""),
                                            "type": img.get("type", "temp")})
                data = http("GET", f"/view?{q}", raw=True, timeout=60)
                fn = out_dir / f"output-node{nid.replace(':', '_')}.png"
                fn.write_bytes(data)
                saved.append(str(fn.relative_to(HERE)))

    lines = [e for e in comfy_logs() if e.get("t", "") >= log_mark]
    text_lines = [ln for e in lines for ln in e.get("m", "").splitlines() if ln.strip()]
    matches = [ln for ln in text_lines if OOM_PATTERN.search(ln)]
    events_only = [ln for ln in matches if not CONFIG_ONLY.search(ln)]

    window = (t_start or t_submit, (t_end or time.time()))
    smi_in = [v for t, v in smi.samples if window[0] - 0.05 <= t <= window[1] + 0.05]
    proc_series = {}
    for label in pdh_targets:
        vals = [row[label] for t, row in pdh.samples if window[0] - 0.05 <= t <= window[1] + 0.05]
        proc_series[label] = {"peak_mib": max(vals) if vals else None, "stats": series(vals),
                              "raw": [round(x, 1) for x in vals]}
        for k in EXTRA_KINDS:
            ev = [row[label] for t, row in pdh.extra[k] if window[0] - 0.05 <= t <= window[1] + 0.05]
            key = k.lower().replace(" ", "_")
            proc_series[label][f"{key}_peak_mib"] = max(ev) if ev else None
            proc_series[label][f"{key}_min_mib"] = min(ev) if ev else None
            proc_series[label][f"{key}_before_submit_mib"] = pdh.extra[k][0][1][label] if pdh.extra[k] else None
    wall_ms = ((t_last_executed or t_end) - t_start) * 1000 if (t_start and (t_last_executed or t_end)) else None
    return {
        "run_index": run_index,
        "prompt_id": prompt_id,
        "status": status,
        "error": error,
        "node_errors": node_errors,
        "wall_ms": wall_ms,
        "wall_ms_definition": "execution_start -> last 'executed' message (websocket receive times)",
        "submit_to_start_ms": (t_start - t_submit) * 1000 if t_start else None,
        "start_to_success_ms": (t_end - t_start) * 1000 if (t_start and t_end) else None,
        "node_ms": {k: round(v, 1) for k, v in node_ms.items()},
        "node_class": {k: v["class_type"] for k, v in graph.items()},
        "seed": args.seed + run_index,
        "image_upload": image_name,
        "mask_upload": mask_name,
        "vram_total_mib": {"source": "nvidia-smi memory.used, 100 ms", "peak": max(smi_in) if smi_in else None,
                           "baseline_before_submit": smi.samples[0][1] if smi.samples else None,
                           "stats": series(smi_in), "raw": smi_in},
        "vram_process_mib": proc_series,
        "vram_snapshot_before_submit": snapshot,
        "pdh_adapter_peak_mib": max((v for t, v in pdh.adapter if window[0] <= t <= window[1]), default=None),
        "sampler_errors": {"nvidia-smi": smi.error, "pdh": pdh.error},
        "log_matches": matches,
        "log_matches_events": events_only,
        "log_lines": text_lines,
        "previews": {"count": len(previews), "frames": previews,
                     "max_side": max((max(p["size"]) for p in previews if p["size"]), default=None)},
        "outputs": {k: v for k, v in outputs.items()},
        "saved_outputs": saved,
        "events": events,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--workflow", required=True, help="name in workflows/ without .json")
    ap.add_argument("--model", choices=sorted(MODELS), help="fills {{MODEL}}")
    ap.add_argument("--lora", help="LoRA file name in models/loras; fills {{LORA}} (omitted: LoRA node removed)")
    ap.add_argument("--lora-strength", type=float, default=1.0)
    ap.add_argument("--runs", type=int, default=1)
    ap.add_argument("--preview", default="taesd", choices=["none", "auto", "latent2rgb", "taesd", "default"],
                    help="extra_data.preview_method (IMG-045)")
    ap.add_argument("--save-previews", action="store_true")
    ap.add_argument("--save-outputs", action="store_true", help="download output images into the run folder")
    ap.add_argument("--seed", type=int, default=1000, help="KSampler seed of run 0; run i uses seed+i")
    ap.add_argument("--image", help="input image uploaded per run for {{IMAGE}}")
    ap.add_argument("--mask", help="mask image uploaded per run for {{MASK}}")
    ap.add_argument("--text", default="", help="fills {{PROMPT}}")
    ap.add_argument("--watch-proc", default="", help="comma-separated process image names to sample with PDH")
    ap.add_argument("--free-first", action="store_true", help="POST /free (unload + free) before run 0")
    ap.add_argument("--tag", default="", help="label written into the result file names")
    ap.add_argument("--timeout", type=float, default=600)
    args = ap.parse_args()

    try:
        http("GET", "/system_stats", timeout=5)
    except (urllib.error.URLError, ConnectionError, OSError, TimeoutError):
        unreachable()

    wf = WORKFLOWS / f"{args.workflow}.json"
    template = json.loads(wf.read_text(encoding="utf-8"))
    stats = http("GET", "/system_stats")
    global LUID
    LUID, adapters, smi_now = detect_luid()
    pid = listening_pid(PORT)
    targets = {"comfyui": [pid] if pid else []}
    names = [n.strip().lower() for n in args.watch_proc.split(",") if n.strip()]
    found = pids_by_name(names)
    for name in names:
        hits = [v for k, v in found.items() if k.lower() == name]
        targets[name] = hits[0] if hits else []

    if args.free_first:
        http("POST", "/free", {"unload_models": True, "free_memory": True})
        time.sleep(3)

    RESULTS.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    label = "-".join(x for x in [args.workflow, args.model, ("lora" if args.lora else ("nolora" if "lora" in args.workflow else "")),
                                 args.tag] if x)
    client_id = uuid.uuid4().hex
    rc = 0
    for i in range(args.runs):
        out_dir = RESULTS / "runs" / f"{label}-{stamp}-r{i + 1}"
        res = one_run(args, template, i, client_id, targets, out_dir, stamp)
        res.update({
            "workflow": args.workflow, "model": args.model, "model_file": MODELS.get(args.model),
            "lora": args.lora, "lora_strength": args.lora_strength if args.lora else None,
            "preview_method": args.preview, "run": i + 1, "runs": args.runs, "tag": args.tag,
            "timestamp": datetime.now().isoformat(), "comfyui_pid": pid, "watched_pids": targets,
            "system_stats": stats if i == 0 else None, "free_first": args.free_first and i == 0,
            "pdh_adapter_luid": LUID, "pdh_adapters_at_start_mib": adapters, "smi_at_start_mib": smi_now,
            "command": " ".join(sys.argv),
        })
        fn = RESULTS / f"{label}-{stamp}-r{i + 1}.json"
        fn.write_text(json.dumps(res, indent=1), encoding="utf-8")
        peak = res["vram_total_mib"]["peak"]
        procs = ", ".join(f"{k} {v['peak_mib']:.0f} (local {v['local_peak_mib'] or 0:.0f})"
                          for k, v in res["vram_process_mib"].items() if v["peak_mib"] is not None)
        wall = f"{res['wall_ms']:.0f} ms" if res["wall_ms"] is not None else "n/a"
        print(f"run {i + 1}/{args.runs}: {res['status']} wall {wall}, peak total {peak} MiB, per-process peak MiB: {procs}; "
              f"previews {res['previews']['count']} (max side {res['previews']['max_side']}); "
              f"OOM/offload event lines {len(res['log_matches_events'])} -> {fn.name}")
        if res["node_errors"]:
            print("  node_errors:", json.dumps(res["node_errors"])[:500])
        if res["status"] != "success":
            print("  error:", res["error"])
            rc = 1
    sys.exit(rc)


if __name__ == "__main__":
    main()
