"""Builds the two UI-format fixtures for the webengine spike (Task 6).

  zimage_t2i.ui.json      the built-in Z-Image Turbo template (comfyui-workflow-templates 0.11.78,
                          image_z_image_turbo.json), byte-for-byte. It already holds a subgraph
                          ("Text to Image (Z-Image-Turbo)", node 57).
  subgraph_muted.ui.json  the same template with seed 1234 (KSampler 3 in the subgraph) plus two
                          muted nodes (mode 2):
                          - node 62 PreviewImage at top level, fed from the subgraph's IMAGE output (link 76);
                          - node 63 LatentUpscaleBy inside the subgraph, fed from KSampler 3 (link 77).
                          The edits mirror what the 1.53.10 frontend serialized when the same nodes were
                          added through LiteGraph in the browser (ids, link ids, slots).

Run: python spikes/webengine/fixtures/make_fixtures.py [path-to-image_z_image_turbo.json]
"""
import copy
import json
import os
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
# ComfyUI venv under the user profile (%USERPROFILE%\ComfyUI\.venv).
DEFAULT_TEMPLATE = (
    pathlib.Path(os.environ["USERPROFILE"]) / "ComfyUI" / ".venv" / "Lib" / "site-packages"
    / "comfyui_workflow_templates_json" / "templates" / "image_z_image_turbo.json")


def main() -> None:
    src = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_TEMPLATE
    raw = src.read_bytes()
    (HERE / "zimage_t2i.ui.json").write_bytes(raw)

    g = json.loads(raw.decode("utf-8"))
    m = copy.deepcopy(g)
    m["id"] = "7d3c6a52-6f0e-4d4b-9b39-77c1f6a1e0b2"
    note = next(n for n in m["nodes"] if n["id"] == 35)
    note["widgets_values"] = [
        "Worldwright webengine spike (Task 6) fixture: the Z-Image Turbo template plus a muted "
        "PreviewImage at top level (node 62) and a muted LatentUpscaleBy inside the subgraph (node 63)."]

    # Top level: muted PreviewImage fed from the subgraph node's IMAGE output.
    sgnode = next(n for n in m["nodes"] if n["id"] == 57)
    sgnode["outputs"][0]["links"] = list(sgnode["outputs"][0].get("links") or []) + [76]
    m["nodes"].append({
        "id": 62, "type": "PreviewImage", "pos": [630, 600], "size": [140, 26], "flags": {},
        "order": 2, "mode": 2,
        "inputs": [{"localized_name": "images", "name": "images", "type": "IMAGE", "link": 76}],
        "outputs": [],
        "properties": {"Node name for S&R": "PreviewImage"},
        "widgets_values": []})
    m["links"].append([76, 57, 0, 62, 0, "IMAGE"])
    m["last_node_id"] = 63
    m["last_link_id"] = 77

    # Inside the subgraph: muted LatentUpscaleBy fed from KSampler 3.
    sg = m["definitions"]["subgraphs"][0]
    ks = next(n for n in sg["nodes"] if n["id"] == 3)
    # A different seed (KSampler widget 0), so the two fixtures convert to different API prompts
    # and a stale conversion result cannot pass for the right one.
    ks["widgets_values"][0] = 1234
    ks["outputs"][0]["links"] = list(ks["outputs"][0].get("links") or []) + [77]
    sg["nodes"].append({
        "id": 63, "type": "LatentUpscaleBy", "pos": [950, 800], "size": [270, 82], "flags": {},
        "order": 9, "mode": 2,
        "inputs": [
            {"localized_name": "samples", "name": "samples", "type": "LATENT", "link": 77},
            {"localized_name": "upscale_method", "name": "upscale_method", "type": "COMBO",
             "widget": {"name": "upscale_method"}, "link": None},
            {"localized_name": "scale_by", "name": "scale_by", "type": "FLOAT",
             "widget": {"name": "scale_by"}, "link": None}],
        "outputs": [{"localized_name": "LATENT", "name": "LATENT", "type": "LATENT", "links": None}],
        "properties": {"Node name for S&R": "LatentUpscaleBy"},
        "widgets_values": ["nearest-exact", 1.5]})
    sg["links"].append({"id": 77, "origin_id": 3, "origin_slot": 0, "target_id": 63,
                        "target_slot": 0, "type": "LATENT"})
    state = sg.setdefault("state", {})
    state["lastNodeId"] = max(state.get("lastNodeId", 0), 63)
    state["lastLinkId"] = max(state.get("lastLinkId", 0), 77)

    (HERE / "subgraph_muted.ui.json").write_text(json.dumps(m, indent=2, ensure_ascii=False) + "\n",
                                                 encoding="utf-8")
    print("wrote", HERE / "zimage_t2i.ui.json", "and", HERE / "subgraph_muted.ui.json")


if __name__ == "__main__":
    main()
