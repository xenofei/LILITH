#!/usr/bin/env python3
"""IMG-160 negative control: one preview run with the TAEF1 decoder hidden, so ComfyUI's log names the
decoder it looks for for Z-Image ("could not find models/vae_approx/taef1_decoder").

Renames every models/vae_approx/taef1_decoder.* to <name>.ww-hidden, runs measure.py with the given
arguments, and renames the files back in a finally block (also on Ctrl+C or a failed run).

  python spikes/comfy-measure/preview_nodecoder.py --workflow zimage_t2i --model int8 --runs 1 \
      --preview taesd --save-previews --tag preview-taesd-nodecoder-cu130

ComfyUI rescans models/vae_approx when the folder's modification time changes, so no restart is needed.
"""
import os
import subprocess
import sys
from pathlib import Path

# ComfyUI install under the user profile (%USERPROFILE%\ComfyUI\ComfyUI).
VAE_APPROX = Path(os.environ["USERPROFILE"]) / "ComfyUI" / "ComfyUI" / "models" / "vae_approx"
MEASURE = Path(__file__).resolve().parent / "measure.py"


def main():
    hidden = []
    try:
        for f in sorted(VAE_APPROX.glob("taef1_decoder.*")):
            if f.suffix == ".ww-hidden":
                continue
            target = f.with_name(f.name + ".ww-hidden")
            f.rename(target)
            hidden.append((target, f))
            print(f"hid {f.name}")
        if not hidden:
            print("no taef1_decoder.* in models/vae_approx; nothing hidden")
        return subprocess.call([sys.executable, str(MEASURE)] + sys.argv[1:])
    finally:
        for target, original in hidden:
            target.rename(original)
            print(f"restored {original.name}")


if __name__ == "__main__":
    sys.exit(main())
