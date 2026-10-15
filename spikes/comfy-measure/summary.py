#!/usr/bin/env python3
"""Prints the Task 8 decision tables from results/*.json (written by measure.py).

  summary.py --fit [--tag fit-cu128]        IMG-155/111 fit table and the verbatim rule outcome of that series
  summary.py --lora [--tag cu128]           IMG-157 SSIM, overhead and the int8 LoRA flag (needs numpy + Pillow)
  summary.py --sam [--tag cu128]            IMG-158 cold/warm latency (and the interleaved series)
  summary.py --previews [--tag cu128]       IMG-160 preview facts
  summary.py --torch                        cu128 vs cu130 timings (all series that exist for both)
  --json <file>                             also write everything printed as JSON

Timings in ms, memory in MiB. Run it with any Python that has numpy and Pillow for --lora (e.g. ComfyUI's
venv); the other tables need the standard library only.
"""
import argparse
import glob
import json
import re
import statistics
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RES = HERE / "results"
FIT_PEAK_MIB = 15500
OUT = {}


def runs(prefix):
    """Result files for one series (a measure.py invocation label), grouped by invocation stamp."""
    files = sorted(glob.glob(str(RES / f"{prefix}-2*-r*.json")))
    groups = {}
    for f in files:
        m = re.search(r"-(\d{8}-\d{6})-r(\d+)\.json$", f)
        groups.setdefault(m.group(1), []).append((int(m.group(2)), f))
    out = []
    for stamp in sorted(groups):
        out.append([json.loads(Path(f).read_text(encoding="utf-8")) for _, f in sorted(groups[stamp])])
    return out


def last_series(prefix):
    s = runs(prefix)
    return s[-1] if s else []


def pct(values, p):
    v = sorted(values)
    if not v:
        return None
    k = (len(v) - 1) * p
    f = int(k)
    c = min(f + 1, len(v) - 1)
    return v[f] + (v[c] - v[f]) * (k - f)


def fmt(x, nd=0):
    return "n/a" if x is None else (f"{x:.{nd}f}" if isinstance(x, float) else str(x))


def proc(r, label, key="local_peak_mib"):
    v = (r.get("vram_process_mib") or {}).get(label) or {}
    return v.get(key)


def ksampler_ms(r):
    return sum(v for k, v in r["node_ms"].items() if r["node_class"].get(k) == "KSampler")


# ------------------------------------------------------------------------------------------------- fit
def fit(tag):
    print(f"## Fit (IMG-155/111): zimage_inpaint_cn_lite, tag {tag}\n")
    verdicts = {}
    for model in ("bf16", "int8"):
        series = last_series(f"zimage_inpaint_cn_lite-{model}-{tag}")
        if not series:
            print(f"{model}: no runs\n")
            continue
        walls = [r["wall_ms"] for r in series]
        med = statistics.median(walls[1:5]) if len(walls) >= 5 else None
        print(f"### {model} ({len(series)} runs; median of runs 2-5 = {fmt(med)} ms; 3x = {fmt(3 * med if med else None)} ms)\n")
        print("| Run | Wall ms | Wall / median(2-5) | KSampler ms | Peak total MiB (nvidia-smi) | ComfyUI dedicated peak | ComfyUI shared (pinned/sysmem) peak | canvas local peak / min | canvas committed | WebEngine local peak | WebEngine committed | WebEngine 'Dedicated Usage' peak | OOM/offload event lines |")
        print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
        rows = []
        for r in series:
            ratio = r["wall_ms"] / med if med else None
            ev = len(r["log_matches_events"])
            row = {"run": r["run"], "wall_ms": round(r["wall_ms"]), "ratio": round(ratio, 2) if ratio else None,
                   "ksampler_ms": round(ksampler_ms(r)), "peak_total_mib": r["vram_total_mib"]["peak"],
                   "comfyui_dedicated_peak_mib": round(proc(r, "comfyui", "peak_mib") or 0),
                   "comfyui_shared_peak_mib": round(proc(r, "comfyui", "shared_peak_mib") or 0),
                   "canvas_local_peak_mib": proc(r, "canvas.exe"), "canvas_local_min_mib": proc(r, "canvas.exe", "local_min_mib"),
                   "canvas_committed_mib": proc(r, "canvas.exe", "total_committed_peak_mib"),
                   "webengine_local_peak_mib": proc(r, "webengine.exe"),
                   "webengine_committed_mib": proc(r, "webengine.exe", "total_committed_peak_mib"),
                   "webengine_dedicated_counter_peak_mib": proc(r, "webengine.exe", "peak_mib"),
                   "event_lines": ev, "config_only_lines": len(r["log_matches"]) - ev, "status": r["status"]}
            rows.append(row)
            print(f"| {r['run']} | {row['wall_ms']} | {fmt(row['ratio'], 2)} | {row['ksampler_ms']} | {row['peak_total_mib']} | "
                  f"{row['comfyui_dedicated_peak_mib']} | {row['comfyui_shared_peak_mib']} | "
                  f"{fmt(row['canvas_local_peak_mib'])} / {fmt(row['canvas_local_min_mib'])} | {fmt(row['canvas_committed_mib'])} | "
                  f"{fmt(row['webengine_local_peak_mib'])} | {fmt(row['webengine_committed_mib'])} | "
                  f"{fmt(row['webengine_dedicated_counter_peak_mib'])} | {ev} |")
        c1 = all(r["peak_total_mib"] is not None and r["peak_total_mib"] <= FIT_PEAK_MIB for r in rows)
        c2 = all(r["event_lines"] == 0 for r in rows)
        c3 = med is not None and all(r["wall_ms"] <= 3 * med for r in rows)
        ok = all(r["status"] == "success" for r in rows) and len(rows) == 5
        fits = c1 and c2 and c3 and ok
        verdicts[model] = fits
        print(f"\n- peak total <= {FIT_PEAK_MIB} MiB in all runs: {'yes' if c1 else 'NO'} (max {max(r['peak_total_mib'] for r in rows)})")
        print(f"- no OOM/offload event line: {'yes' if c2 else 'NO'}")
        print(f"- every run <= 3 x median(runs 2-5): {'yes' if c3 else 'NO'} (max ratio {max(r['ratio'] for r in rows)})")
        print(f"- **{model} fits: {'YES' if fits else 'NO'}**\n")
        OUT.setdefault("fit", {})[f"{model}-{tag}"] = {"rows": rows, "median_2_5_ms": med, "c_peak": c1,
                                                       "c_log": c2, "c_time": c3, "fits": fits}
    if verdicts.get("bf16"):
        outcome = "bf16 + lite ControlNet (bf16 fits)"
    elif verdicts.get("int8"):
        outcome = "int8 + lite ControlNet (bf16 does not fit, int8 fits)"
    else:
        outcome = "FAIL: neither fits (rule's follow-up: nvfp4)"
    print(f"Verbatim rule outcome for this series only: {outcome}\n")
    print("Note: under ComfyUI's DynamicVRAM the peak and log criteria do not discriminate (run-1 peaks of both "
          "precisions sit within about +-60 MiB of 15,500; offload is never logged), so one series' outcome is not "
          "the IMG-111 decision. IMG-111 decision (controller ruling R7): int8 + lite ControlNet, the IMG-111 default, "
          "because bf16 was not robustly shown to fit; see docs/spikes/08-comfy-measurements.md section 7.\n")
    OUT.setdefault("fit", {})[f"rule-outcome-{tag}"] = outcome
    OUT["fit"]["img111_decision"] = "int8 + lite ControlNet (IMG-111 default; bf16 not robustly shown to fit; ruling R7)"


# ------------------------------------------------------------------------------------------------ lora
def ssim(a, b):
    import numpy as np
    def lum(path):
        from PIL import Image
        im = np.asarray(Image.open(path).convert("RGB"), dtype=np.float64)
        return im[..., 0] * 0.299 + im[..., 1] * 0.587 + im[..., 2] * 0.114
    x, y = lum(a), lum(b)
    r = 5
    g = np.exp(-(np.arange(-r, r + 1) ** 2) / (2 * 1.5 ** 2))
    g /= g.sum()
    def blur(z):
        h = sum(g[i] * z[:, i:z.shape[1] - 2 * r + i] for i in range(2 * r + 1))
        return sum(g[i] * h[i:h.shape[0] - 2 * r + i, :] for i in range(2 * r + 1))
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    mx, my = blur(x), blur(y)
    sxx, syy, sxy = blur(x * x) - mx * mx, blur(y * y) - my * my, blur(x * y) - mx * my
    m = ((2 * mx * my + c1) * (2 * sxy + c2)) / ((mx * mx + my * my + c1) * (sxx + syy + c2))
    return float(m.mean())


def out_png(r):
    p = (r.get("saved_outputs") or [None])[0]
    return HERE / p if p else None


def lora(tag):
    print(f"## LoRA on int8 (IMG-157): zimage_t2i_lora, tag {tag}\n")
    s = {k: last_series(f"zimage_t2i_lora-{k}-{tag}") for k in ("bf16-lora", "int8-lora", "bf16-nolora", "int8-nolora")}
    if not all(s.values()):
        print("missing series:", [k for k, v in s.items() if not v])
        return
    lora_name = s["bf16-lora"][0]["lora"]
    print(f"LoRA: {lora_name}, strength {s['bf16-lora'][0]['lora_strength']}; seeds {[r['seed'] for r in s['bf16-lora']]}\n")
    print("| Seed | SSIM(bf16 no-LoRA, bf16 LoRA) | SSIM(int8 no-LoRA, int8 LoRA) | SSIM(bf16 LoRA, int8 LoRA) | SSIM(bf16 no-LoRA, int8 no-LoRA) |")
    print("|---|---|---|---|---|")
    rows = []
    for i in range(min(len(v) for v in s.values())):
        seeds = {k: v[i]["seed"] for k, v in s.items()}
        assert len(set(seeds.values())) == 1, seeds
        p = {k: out_png(v[i]) for k, v in s.items()}
        row = {"seed": seeds["bf16-lora"],
               "bf16_change": ssim(p["bf16-nolora"], p["bf16-lora"]),
               "int8_change": ssim(p["int8-nolora"], p["int8-lora"]),
               "agree_lora": ssim(p["bf16-lora"], p["int8-lora"]),
               "agree_nolora": ssim(p["bf16-nolora"], p["int8-nolora"])}
        rows.append(row)
        print(f"| {row['seed']} | {row['bf16_change']:.4f} | {row['int8_change']:.4f} | {row['agree_lora']:.4f} | {row['agree_nolora']:.4f} |")
    print("\n| Config | Wall ms per run | Median runs 2-3 | KSampler ms runs 2-3 |")
    print("|---|---|---|---|")
    med = {}
    for k, v in s.items():
        walls = [r["wall_ms"] for r in v]
        med[k] = statistics.median(walls[1:3])
        print(f"| {k} | {', '.join(str(round(w)) for w in walls)} | {med[k]:.0f} | {', '.join(str(round(ksampler_ms(r))) for r in v[1:3])} |")
    ov = {m: (med[f"{m}-lora"] / med[f"{m}-nolora"] - 1) * 100 for m in ("bf16", "int8")}
    c1 = all(r["bf16_change"] < 0.95 and r["int8_change"] < 0.95 for r in rows)
    c2 = all(r["agree_lora"] >= 0.80 for r in rows)
    c3 = all(v <= 20 for v in ov.values())
    flag = "supported" if (c1 and c2 and c3) else "unsupported"
    print(f"\n- LoRA visibly changes the output, SSIM(no-LoRA, LoRA) < 0.95 on both precisions for every seed: {'yes' if c1 else 'NO'} "
          f"(max bf16 {max(r['bf16_change'] for r in rows):.4f}, max int8 {max(r['int8_change'] for r in rows):.4f})")
    print(f"- precisions agree, SSIM(bf16+LoRA, int8+LoRA) >= 0.80 for every seed: {'yes' if c2 else 'NO'} (min {min(r['agree_lora'] for r in rows):.4f})")
    print(f"- LoRA time overhead <= 20 %: {'yes' if c3 else 'NO'} (bf16 {ov['bf16']:+.1f} %, int8 {ov['int8']:+.1f} %)")
    print(f"\n**int8 profile LoRA flag (IMG-107/157): `{flag}`**\n")
    OUT["lora"] = {"tag": tag, "lora": lora_name, "rows": rows, "median_ms": med, "overhead_pct": ov,
                   "c_change": c1, "c_agree": c2, "c_overhead": c3, "flag": flag}


# ------------------------------------------------------------------------------------------------- sam
def sam(tag):
    print(f"## SAM 3.1 select object (IMG-158): sam31_select, tag {tag}\n")
    series = last_series(f"sam31_select-{tag}")
    if series:
        cold, warm = series[0], series[1:]
        def node(r, cls):
            return sum(v for k, v in r["node_ms"].items() if r["node_class"].get(k) == cls)
        ww = [r["wall_ms"] for r in warm]
        wd = [node(r, "SAM3_Detect") for r in warm]
        print("| Series | n | p50 wall ms | max wall ms | p50 SAM3_Detect ms | Z-Image resident at submit |")
        print("|---|---|---|---|---|---|")
        print(f"| cold (run 1) | 1 | {cold['wall_ms']:.0f} | {cold['wall_ms']:.0f} | {node(cold, 'SAM3_Detect'):.0f} | "
              f"yes ({cold['vram_total_mib']['baseline_before_submit']} MiB in use) |")
        bases = [r["vram_total_mib"]["baseline_before_submit"] for r in warm]
        print(f"| warm (runs 2-{len(series)}) | {len(warm)} | {pct(ww, 0.5):.0f} | {max(ww):.0f} | {pct(wd, 0.5):.0f} | "
              f"no: {min(bases)}-{max(bases)} MiB in use (ComfyUI dropped Z-Image after run 1) |")
        OUT["sam"] = {"tag": tag, "cold_ms": cold["wall_ms"], "warm_p50_ms": pct(ww, 0.5), "warm_max_ms": max(ww),
                      "warm_detect_p50_ms": pct(wd, 0.5), "warm_n": len(warm), "cold_node_ms": cold["node_ms"],
                      "target_warm_p50_ms": 1500, "meets_target": pct(ww, 0.5) <= 1500}
    inter = runs(f"sam31_select-interleave-{tag}")
    t2i = runs(f"zimage_t2i-*-interleave-{tag}")
    if inter:
        iw = [s[0]["wall_ms"] for s in inter]
        tw = [s[0]["wall_ms"] for s in t2i]
        print(f"| interleaved (t2i then SAM, x{len(iw)}) | {len(iw)} | {pct(iw, 0.5):.0f} | {max(iw):.0f} | "
              f"{pct([sum(v for k, v in s[0]['node_ms'].items() if s[0]['node_class'].get(k) == 'SAM3_Detect') for s in inter], 0.5):.0f} | yes |")
        model = t2i[0][0]["model"] if t2i else "-"
        print(f"\nInterleaved Z-Image t2i runs ({model}; runs 2-5 each follow a SAM run, so Z-Image reloads): "
              f"{', '.join(str(round(w)) for w in tw)} ms")
        OUT.setdefault("sam", {})["interleaved_sam_ms"] = iw
        OUT["sam"]["interleaved_t2i_ms"] = tw
    print()


# -------------------------------------------------------------------------------------------- previews
def previews(tag):
    print(f"## Previews (IMG-160), tag {tag}\n")
    print("| preview_method | frames | frame size | JPEG bytes (first) | wall ms | KSampler ms | decoder (code path / log) |")
    print("|---|---|---|---|---|---|---|")
    rows = {}
    for label, note in (("preview-taesd", "TAESD previewer with models/vae_approx/taef1_decoder.pth (Flux latent format)"),
                        ("preview-taesd-warm", "same, warm runs"),
                        ("preview-auto", "Latent2RGB (auto maps to Latent2RGB in latent_preview.get_previewer)"),
                        ("preview-none", "none"),
                        ("preview-taesd-nodecoder", "Latent2RGB fallback; log warns 'could not find models/vae_approx/taef1_decoder'"),
                        ("preview-none-warm", "none, warm runs"),
                        ("preview-auto-warm", "Latent2RGB, warm runs")):
        for r in last_series(f"zimage_t2i-*-{label}-{tag}"):
            p = r["previews"]
            sizes = sorted({tuple(f["size"]) for f in p["frames"] if f["size"]})
            first = p["frames"][0]["bytes"] if p["frames"] else None
            warn = [l for l in r["log_lines"] if "TAESD" in l or "vae_approx" in l]
            print(f"| {r['preview_method']} ({r['model']} {label} r{r['run']}) | {p['count']} | {', '.join('x'.join(map(str, s)) for s in sizes) or '-'} | "
                  f"{fmt(first)} | {r['wall_ms']:.0f} | {ksampler_ms(r):.0f} | {note} |")
            rows.setdefault(label, []).append({"run": r["run"], "count": p["count"], "sizes": sizes, "wall_ms": r["wall_ms"],
                                               "ksampler_ms": ksampler_ms(r), "log": warn})
    print()
    OUT["previews"] = rows


# --------------------------------------------------------------------------------------------- torch
def torch_cmp():
    print("## cu128 vs cu130\n")
    print("| Series | cu128 wall ms (runs) | cu128 median 2-5 | cu130 wall ms (runs) | cu130 median 2-5 | speed-up |")
    print("|---|---|---|---|---|---|")
    rows = []
    for base in ("zimage_inpaint_cn_lite-bf16-fit", "zimage_inpaint_cn_lite-int8-fit",
                 "zimage_inpaint_cn_lite-bf16-nocomp", "zimage_inpaint_cn_lite-int8-nocomp"):
        a, b = last_series(f"{base}-cu128"), last_series(f"{base}-cu130")
        if not a or not b:
            continue
        ma, mb = statistics.median([r["wall_ms"] for r in a][1:5]), statistics.median([r["wall_ms"] for r in b][1:5])
        rows.append({"series": base, "cu128_median_ms": ma, "cu130_median_ms": mb, "speedup": ma / mb})
        print(f"| {base} | {', '.join(str(round(r['wall_ms'])) for r in a)} | {ma:.0f} | "
              f"{', '.join(str(round(r['wall_ms'])) for r in b)} | {mb:.0f} | {ma / mb:.2f}x |")
    print()
    OUT["torch"] = rows


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fit", action="store_true")
    ap.add_argument("--lora", action="store_true")
    ap.add_argument("--sam", action="store_true")
    ap.add_argument("--previews", action="store_true")
    ap.add_argument("--torch", action="store_true")
    ap.add_argument("--tag", default=None)
    ap.add_argument("--json")
    a = ap.parse_args()
    if not any([a.fit, a.lora, a.sam, a.previews, a.torch]):
        ap.print_help()
        return 1
    if a.fit:
        fit(a.tag or "fit-cu128")
    if a.lora:
        lora(a.tag or "cu128")
    if a.sam:
        sam(a.tag or "cu128")
    if a.previews:
        previews(a.tag or "cu128")
    if a.torch:
        torch_cmp()
    if a.json:
        Path(a.json).write_text(json.dumps(OUT, indent=1, default=str), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
