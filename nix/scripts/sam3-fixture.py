"""Record the SAM 3 interactive masks the cordoned Rust test compares against.

The reference is `Sam3Image.predict_inst` run unwrapped: a box prompt in, the one
object's mask out. Every other check on the interactive path is internal to the
Rust side; this is the only comparison that spans the ONNX graph the crate runs
and the torch model it was traced from, so a wrong box-to-model-frame mapping, a
feature-prep divergence, or a mask upscale off by a convention has somewhere to
show. Whatever `predict_inst` produces is the target.

The boxes come from the text-prompted grounding head (a human judged them once);
here they only drive the geometric prompt, so the text prompt rides along as
provenance and is not replayed. Each box is normalized xyxy and scaled to the
image's own pixels, the frame `predict_inst` expects with `normalize_coords`.

The export's store path goes into the output for the same reason the DINOv3
fixture records it: the export is input-addressed and `torch.onnx.export` is not
bit-reproducible, so putting it in this fixture's closure stops `nix store gc`
from reclaiming a graph the cached fixture still describes.

Usage: sam3-fixture.py <export-dir> <boxes-json> <output-dir>
"""

import json
import shutil
import sys
from pathlib import Path

import numpy as np
import torch
import torchvision
from PIL import Image
from sam3.model.sam3_image_processor import Sam3Processor
from sam3.model_builder import build_sam3_image_model

# Matches the ONNX runtime's pinned intra-op thread count: thread count
# partitions GEMM reductions, the largest same-machine source of numeric drift.
# A thresholded mask hides most of it, but pinning keeps the reference stable.
NUM_THREADS = 1

export_dir, boxes_json, out_dir = (Path(argument) for argument in sys.argv[1:4])

manifest = json.loads((export_dir / "manifest.json").read_text())
decoder_meta = manifest["models"]["decoder_interactive"]["metadata"]

torch.set_num_threads(NUM_THREADS)

model = build_sam3_image_model(device="cpu", enable_inst_interactivity=True)
model.eval()
processor = Sam3Processor(model, device="cpu")

images_dir = out_dir / "images"
images_dir.mkdir(parents=True)
masks_dir = out_dir / "masks"
masks_dir.mkdir(parents=True)

entries = []
for box in json.loads(boxes_json.read_text()):
    entry_id = box["id"]
    source_path = Path(box["path"])
    shutil.copyfile(source_path, images_dir / entry_id)

    image = Image.open(source_path).convert("RGB")
    width, height = image.size
    x0, y0, x1, y1 = box["box_xyxy_norm"]
    box_pixels = np.array([x0 * width, y0 * height, x1 * width, y1 * height])

    state = processor.set_image(image)
    masks, ious, _ = model.predict_inst(
        state, box=box_pixels, multimask_output=True, normalize_coords=True
    )
    best = int(ious.argmax())
    mask = masks[best] > 0.5
    Image.fromarray((mask * 255).astype("uint8"), mode="L").save(
        masks_dir / f"{entry_id}.png"
    )

    entries.append(
        {
            "id": entry_id,
            "prompt": box["prompt"],
            "file": f"images/{entry_id}",
            "mask": f"masks/{entry_id}.png",
            "box_xyxy_norm": box["box_xyxy_norm"],
            "predicted_iou": round(float(ious[best]), 4),
            "source": {"width": width, "height": height},
        }
    )
    print(
        f"  {entry_id:32s} {box['prompt']:14s} {width}x{height}  "
        f"iou {float(ious[best]):.3f}  {int(mask.sum())}px"
    )

(out_dir / "reference.json").write_text(
    json.dumps(
        {
            "export": str(export_dir),
            "num_candidates": decoder_meta["num_candidates"],
            "low_res_mask_size": decoder_meta["low_res_mask_size"],
            # What produced the masks, so a later disagreement reads against the
            # implementation it was measured on.
            "reference": {
                "predictor": "Sam3Image.predict_inst",
                "decoder": "PIL.Image",
                "torch": torch.__version__,
                "torchvision": torchvision.__version__,
                "num_threads": NUM_THREADS,
                "mask_threshold": decoder_meta["mask_threshold"],
            },
            "entries": entries,
        },
        indent=2,
    )
)

print(f"sam3 fixture: {len(entries)} interactive masks")
