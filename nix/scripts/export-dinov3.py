"""Export DINOv3 ViT-L/16 to ONNX with preprocessing baked into the graph.

There is no `AutoImageProcessor` on the Rust side, so the rescale and
normalization constants have to live somewhere. Baking them into the graph
keeps them tied to the checkpoint they belong to; the alternative is
transcribing ImageNet statistics into Rust, where a wrong digit yields
plausible-looking embeddings that are quietly wrong and that nothing downstream
can detect.

Resizing stays with the caller, matching the SAM 3 image encoder's contract:
both graphs take a uint8 CHW image already at the model's native resolution.

Resolution is a parameter because it is the only lever on patch-grid density:
patch size is the 16x16 kernel of the patch-embedding conv, so it belongs to the
checkpoint. DINOv3 uses RoPE rather than learned position embeddings, so other
resolutions need no embedding interpolation. Cost measured on ViT-L is linear in
token count (224px 119ms/201 tokens, 448px 400ms/789, 672px 1052ms/1769).

Usage: export-dinov3.py <model-dir> <output-dir> [resolution]
"""

import json
import sys
from pathlib import Path

import numpy as np
import onnxruntime as ort
import torch
from transformers import AutoModel

OPSET = 18

# PIL resampling filters, by the code `preprocessor_config.json` stores. Naming
# the filter without deriving it from the code lets a checkpoint change one and
# leave the sidecar asserting the other.
PIL_RESAMPLE = {
    0: "nearest",
    1: "lanczos",
    2: "bilinear",
    3: "bicubic",
    4: "box",
    5: "hamming",
}

# fp32 ViT-L over 24 layers; ONNX and torch accumulate differently, but a
# mis-traced graph diverges by far more than this.
TOLERANCE = 1e-3

model_dir, out_root = Path(sys.argv[1]), Path(sys.argv[2])
config = json.loads((model_dir / "config.json").read_text())
preprocess = json.loads((model_dir / "preprocessor_config.json").read_text())

if len(sys.argv) > 3:
    resolution = int(sys.argv[3])
else:
    # Only consulted as a fallback. HF also spells this `{"shortest_edge": N}`,
    # which this export has no square interpretation for.
    size = preprocess["size"]
    if {"width", "height"} - size.keys():
        sys.exit(f"no square default resolution in {size}; pass one explicitly")
    if size["width"] != size["height"]:
        sys.exit(f"default resolution {size} is not square; pass one explicitly")
    resolution = size["height"]

interpolation = PIL_RESAMPLE.get(preprocess["resample"])
if interpolation is None:
    sys.exit(f"unrecognized PIL resample code {preprocess['resample']}")

patch = config["patch_size"]
if resolution % patch:
    sys.exit(f"resolution {resolution} is not a multiple of patch size {patch}")
patch_grid = resolution // patch

# CLS first, then the registers, then the patch tokens. Masked pooling reads the
# patch tokens, so it has to skip this many.
prefix_tokens = 1 + config["num_register_tokens"]


class Dinov3Encoder(torch.nn.Module):
    """uint8 CHW image in, last_hidden_state out."""

    def __init__(self, model: torch.nn.Module) -> None:
        super().__init__()
        self.model = model
        self.register_buffer(
            "mean", torch.tensor(preprocess["image_mean"]).view(3, 1, 1)
        )
        self.register_buffer("std", torch.tensor(preprocess["image_std"]).view(3, 1, 1))
        self.rescale = preprocess["rescale_factor"]

    def forward(self, image: torch.Tensor) -> torch.Tensor:
        pixels = (image.to(torch.float32) * self.rescale - self.mean) / self.std
        return self.model(pixel_values=pixels.unsqueeze(0)).last_hidden_state


model = AutoModel.from_pretrained(model_dir, torch_dtype=torch.float32)
model.eval()
for parameter in model.parameters():
    parameter.requires_grad_(False)

encoder = Dinov3Encoder(model)
encoder.eval()

out_dir = out_root / "dinov3"
out_dir.mkdir(parents=True, exist_ok=True)
graph_path = out_dir / "dinov3.onnx"
# Resizing stays with the caller, so the graph's fixed input resolution is a
# contract the Rust side has to honour; the sidecar carries it.

# Deterministic and structured. An all-zero image drives degenerate activations
# that can hide a lowering bug, and a seeded RNG would make the build's own
# reproducibility depend on torch's generator.
probe = (
    torch.arange(3 * resolution * resolution, dtype=torch.int64)
    .remainder(251)
    .to(torch.uint8)
    .view(3, resolution, resolution)
)

dummy = torch.zeros(3, resolution, resolution, dtype=torch.uint8)
with torch.no_grad():
    torch.onnx.export(
        encoder,
        (dummy,),
        str(graph_path),
        input_names=["image"],
        output_names=["last_hidden_state"],
        opset_version=OPSET,
        do_constant_folding=True,
        dynamo=False,
    )

with torch.no_grad():
    reference = encoder(probe)

expected_sequence = prefix_tokens + patch_grid * patch_grid
if reference.shape[1] != expected_sequence:
    sys.exit(
        f"token count {reference.shape[1]} does not match "
        f"{prefix_tokens} prefix + {patch_grid}x{patch_grid} patches"
    )

# Loading proves the graph is well-formed; only running it proves the trace
# preserved the model. A lowering quirk in sdpa or RoPE would otherwise reach
# the crate as embeddings that look entirely reasonable.
session = ort.InferenceSession(str(graph_path), providers=["CPUExecutionProvider"])
(exported,) = session.run(None, {"image": probe.numpy()})
drift = np.abs(exported - reference.numpy()).max()
if not np.isfinite(drift) or drift > TOLERANCE:
    sys.exit(f"exported graph drifts {drift:.3e} from torch, over {TOLERANCE:.0e}")

(out_dir / "dinov3.json").write_text(
    json.dumps(
        {
            # Where each number below also appears in the graph, so the
            # verifier can hold the two to each other.
            "graph_assertions": [
                {"claim": "resolution", "tensor": "image", "axis": -1},
                {
                    "claim": "sequence_length",
                    "tensor": "last_hidden_state",
                    "axis": 1,
                },
                {"claim": "hidden_size", "tensor": "last_hidden_state", "axis": 2},
            ],
            "resolution": resolution,
            "patch_size": patch,
            "patch_grid": [patch_grid, patch_grid],
            "prefix_tokens": prefix_tokens,
            "hidden_size": config["hidden_size"],
            "sequence_length": expected_sequence,
            "preprocessing": {
                "rescale_factor": preprocess["rescale_factor"],
                "image_mean": preprocess["image_mean"],
                "image_std": preprocess["image_std"],
                "baked_into_graph": True,
                # The caller resizes, so it owns these. The HF processor for
                # this checkpoint resizes to a square with PIL resample code 2
                # and converts to RGB; nearest-neighbour or BGR here produces
                # embeddings that look fine and are wrong.
                "caller_resize": {
                    "interpolation": interpolation,
                    "pil_resample_code": preprocess["resample"],
                    "antialias": True,
                    "channel_order": "rgb",
                    "layout": "chw",
                    "target": [resolution, resolution],
                },
            },
        },
        indent=2,
    )
)

print(f"exported dinov3 at {resolution}px: {list(reference.shape)}")
print(f"  patch grid {patch_grid}x{patch_grid}, {prefix_tokens} prefix tokens")
print(f"  onnx matches torch to {drift:.3e}")
