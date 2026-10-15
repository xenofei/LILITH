#!/usr/bin/env python3
"""Derives the Task 8 API workflows from the frontend's own Export (API) output. Standard library only.

Sources (all produced by ComfyUI frontend 1.53.10's app.graphToPrompt(), i.e. what "Export (API)" writes,
from the built-in templates of comfyui-workflow-templates 0.11.78 served at /templates/<name>.json):
  zimage_t2i.json                                   Task 4's export of image_z_image_turbo (kept unchanged:
                                                    spikes/canvas/load/comfy_load.py and the Task 6 checks use it)
  exports/image_z_image_turbo_fun_union_controlnet.api.json
  exports/utility_image_segment_sam3.api.json

Every edit below is listed; nothing else changes.
"""
import copy
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
CN_LITE = "Z-Image-Turbo-Fun-Controlnet-Union-2.1-lite-2602-8steps.safetensors"
INPAINT_PROMPT = ("A small blue wooden boathouse on the harbor behind a Latina woman with thick wavy hair, "
                  "boats and pastel houses. Breezy seaside light, warm tones, cinematic close-up.")
LORA_TRIGGER = "Pixel art style. "  # tarn59/pixel_art_style_lora_z_image_turbo's instance prompt


def load(name):
    return json.loads((HERE / name).read_text(encoding="utf-8"))


def save(name, graph):
    (HERE / name).write_text(json.dumps(graph, indent=2) + "\n", encoding="utf-8")
    print("wrote", name, len(graph), "nodes")


def inpaint():
    g = load("exports/image_z_image_turbo_fun_union_controlnet.api.json")
    # 1. The template's Canny branch and its preview are not used in inpaint mode.
    del g["56"], g["57"]
    # 2. The input image comes from LoadImage ({{IMAGE}}, uploaded per run); the mask from a new LoadImageMask
    #    ({{MASK}}, red channel: white = repaint).
    g["58"]["inputs"]["image"] = "{{IMAGE}}"
    g["59"] = {"inputs": {"image": "{{MASK}}", "channel": "red"}, "class_type": "LoadImageMask",
               "_meta": {"title": "Load Image (as Mask)"}}
    # 3. The lite 2602 8-step ControlNet instead of the template's full Union file.
    g["70:64"]["inputs"]["name"] = CN_LITE
    # 4. ZImageFunControlnet (the Z-Image node with inpaint_image + mask inputs; same implementation as the
    #    template's QwenImageDiffsynthControlnet, which has no inpaint_image input) in inpaint mode.
    cn = g["70:60"]
    cn["class_type"] = "ZImageFunControlnet"
    cn["_meta"]["title"] = "ZImageFunControlnet"
    del cn["inputs"]["image"]
    cn["inputs"]["inpaint_image"] = ["58", 0]
    cn["inputs"]["mask"] = ["59", 0]
    # 5. Latent size from the input image (the template took it from the Canny image).
    g["70:69"]["inputs"]["image"] = ["58", 0]
    # 6. {{MODEL}} and an inpaint prompt.
    g["70:46"]["inputs"]["unet_name"] = "{{MODEL}}"
    g["70:45"]["inputs"]["text"] = INPAINT_PROMPT
    save("zimage_inpaint_cn_lite.json", g)


def t2i_lora():
    g = load("zimage_t2i.json")
    # 1. {{MODEL}}.
    g["57:28"]["inputs"]["unet_name"] = "{{MODEL}}"
    # 2. LoraLoaderModelOnly between the diffusion model and ModelSamplingAuraFlow ({{LORA}}; measure.py
    #    removes the node again for the no-LoRA runs).
    g["90"] = {"inputs": {"lora_name": "{{LORA}}", "strength_model": 1.0, "model": ["57:28", 0]},
               "class_type": "LoraLoaderModelOnly", "_meta": {"title": "LoraLoaderModelOnly"}}
    g["57:11"]["inputs"]["model"] = ["90", 0]
    # 3. The LoRA's trigger words in front of the template prompt (used with and without the LoRA).
    g["57:27"]["inputs"]["text"] = LORA_TRIGGER + g["57:27"]["inputs"]["text"]
    save("zimage_t2i_lora.json", g)


def sam():
    g = load("exports/utility_image_segment_sam3.api.json")
    # 1. Input image and text prompt as placeholders; everything else as exported.
    g["79"]["inputs"]["image"] = "{{IMAGE}}"
    g["99:78"]["inputs"]["text"] = "{{PROMPT}}"
    save("sam31_select.json", g)


if __name__ == "__main__":
    inpaint()
    t2i_lora()
    sam()
