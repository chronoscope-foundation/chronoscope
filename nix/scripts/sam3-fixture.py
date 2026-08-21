"""Record the SAM 3 masks the cordoned Rust tests compare against.

Two references, both spanning the ONNX graphs the crate runs and the torch model
they were traced from — the only checks that cross that boundary, where every
other check on these paths is internal to the Rust side:

The interactive reference is `Sam3Image.predict_inst` run unwrapped: a box prompt
in, the one object's mask out, so a wrong box-to-model-frame mapping, a
feature-prep divergence, or a mask upscale off by a convention has somewhere to
show. Whatever `predict_inst` produces is the target. Each box is normalized xyxy
and scaled to the image's own pixels, the frame `predict_inst` expects with
`normalize_coords`.

The concept reference is `set_text_prompt` -> `_forward_grounding`, the
text-detection path `Sam3.segment_concept` traces: the same images and their
prompts in, every instance the grounding head finds out. It is what catches a
tokenizer or a grounding-decoder wiring bug the interactive path never exercises.

The boxes were seeded from the text-prompted grounding head (a human judged them
once), so the two references share images and prompts.

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
from PIL import Image
from sam3.model.sam3_image_processor import Sam3Processor
from sam3.model_builder import build_sam3_image_model

# Pin the torch thread count so the recorded masks are reproducible across
# builds. The masks are thresholded, so this rarely moves a pixel, but a stable
# reference costs nothing.
NUM_THREADS = 1

export_dir, boxes_json, out_dir = (Path(argument) for argument in sys.argv[1:4])

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

concept_masks_dir = out_dir / "concept_masks"
concept_masks_dir.mkdir(parents=True)

# The concept (text-prompt) reference: the same images and prompts run through the
# grounding head the Rust `segment_concept` traces, via `set_text_prompt` ->
# `_forward_grounding`. It returns every instance the model finds at its 0.5
# confidence floor, each mask already boolean at the source grid. The union of
# those masks and the instance count are what the Rust comparison reproduces: the
# union is robust to a boundary pixel, the count catches a split or a merge the
# union would hide. A fresh state per image keeps the interactive loop's box
# prompt from leaking into the text-only grounding.
concept_entries = []
for box in json.loads(boxes_json.read_text()):
    entry_id = box["id"]
    prompt = box["prompt"]
    image = Image.open(Path(box["path"])).convert("RGB")
    width, height = image.size

    state = processor.set_image(image)
    result = processor.set_text_prompt(prompt, state)
    masks = result["masks"]
    scores = result["scores"]
    # Count instances the way the Rust side does: those with any pixel after the
    # 0.5 mask threshold. A kept-but-empty instance segments nothing and is
    # dropped on both sides, so counting it here would desync the comparison.
    count = int(masks.flatten(1).any(dim=1).sum())
    if masks.shape[0] > 0:
        union = masks.any(dim=0)[0].cpu().numpy()
    else:
        union = np.zeros((height, width), dtype=bool)
    Image.fromarray((union * 255).astype("uint8"), mode="L").save(
        concept_masks_dir / f"{entry_id}.png"
    )

    concept_entries.append(
        {
            "id": entry_id,
            "file": f"images/{entry_id}",
            "prompt": prompt,
            "count": count,
            "union_mask": f"concept_masks/{entry_id}.png",
            "scores": [round(float(score), 4) for score in scores.tolist()],
            "source": {"width": width, "height": height},
        }
    )
    print(
        f"  concept {entry_id:24s} {prompt:14s} {count} instance(s)  {int(union.sum())}px"
    )

(out_dir / "reference.json").write_text(
    json.dumps(
        {
            "export": str(export_dir),
            "entries": entries,
            "concept": concept_entries,
        },
        indent=2,
    )
)

print(
    f"sam3 fixture: {len(entries)} interactive masks, "
    f"{len(concept_entries)} concept references"
)
