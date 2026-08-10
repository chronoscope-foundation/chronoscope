"""Record the DINOv3 embeddings the cordoned Rust tests compare against.

The reference is the checkpoint's own `AutoImageProcessor` and `AutoModel`, run
unwrapped. Every other check in this chain compares two readings of
`preprocessor_config.json` that share an author, so a misread rescale convention
or op order would agree with itself all the way to embeddings that look fine and
are wrong. Calling the processor is the only comparison that spans two
languages and two ecosystems.

Whatever the processor does is therefore the target, including its own resize;
reimplementing any of it here would restore the shared author. Resolution is the
one override, since the export chooses it and the checkpoint only carries a
default.

Decoding is torchvision's, the library the processor already runs its transforms
through. Rust brings its own JPEG decoder regardless, so no Python decoder is the
one to match; decode drift is a term in the comparison's budget rather than
something to engineer away.

The export's store path goes into the output, which puts the export in this
fixture's runtime closure. The export is input-addressed and `torch.onnx.export`
is not bit-reproducible, so otherwise `nix store gc` can reclaim the graph while
this fixture survives; the rebuild lands on the same store path carrying
different bytes, and the cached fixture then describes a graph that no longer
exists.

Usage: dinov3-fixture.py <model-dir> <export-dir> <images-json> <output-dir>
"""

import json
import shutil
import sys
from pathlib import Path

import torch
import torchvision
import transformers
from torchvision.io import decode_image
from transformers import AutoImageProcessor, AutoModel

# Thread count partitions GEMM reductions, making it the largest same-machine
# source of drift in these numbers. The Rust side pins its own runtime to one
# thread for the same reason.
NUM_THREADS = 1

model_dir, export_dir, images_json, out_dir = (
    Path(argument) for argument in sys.argv[1:5]
)

manifest = json.loads((export_dir / "manifest.json").read_text())
metadata = manifest["models"]["dinov3"]["metadata"]
resolution = metadata["resolution"]
prefix_tokens = metadata["prefix_tokens"]
hidden_size = metadata["hidden_size"]
grid_rows, grid_columns = metadata["patch_grid"]

torch.set_num_threads(NUM_THREADS)

processor = AutoImageProcessor.from_pretrained(model_dir)
model = AutoModel.from_pretrained(model_dir, dtype=torch.float32)
model.eval()

size = {"height": resolution, "width": resolution}


def unit(vector: torch.Tensor) -> torch.Tensor:
    return vector / vector.norm()


def embed(image: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
    """The normalized CLS embedding and the raw patch grid of one image."""
    batch = processor(image, size=size, return_tensors="pt")
    with torch.no_grad():
        tokens = model(pixel_values=batch["pixel_values"]).last_hidden_state[0]
    return unit(tokens[0]), tokens[prefix_tokens:]


def digits(vector: torch.Tensor) -> list[float]:
    """float32 round-trips through 9 significant digits; the rest is noise."""
    return [float(f"{component:.9g}") for component in vector.tolist()]


images_dir = out_dir / "images"
images_dir.mkdir(parents=True)

# The full patch grid rides along as a binary sidecar per image rather than in
# reference.json: at 784 patches x 1024 it is megabytes of floats, and the Rust
# side compares it as raw bytes anyway.
patches_dir = out_dir / "patches"
patches_dir.mkdir(parents=True)

references = []
for source in json.loads(images_json.read_text()):
    source_path = Path(source["path"])
    shutil.copyfile(source_path, images_dir / source["id"])

    decoded = decode_image(str(source_path))
    channels, height, width = decoded.shape

    reference_cls, reference_patches = embed(decoded)

    # Explicit little-endian: torch/numpy `tofile` writes native order, which is
    # LE by coincidence on today's hosts, not by contract. `<f4` pins it so the
    # Rust reader's `from_le_bytes` is always right.
    patches_file = f"patches/{source['id']}.f32"
    (out_dir / patches_file).write_bytes(
        reference_patches.numpy().astype("<f4").tobytes()
    )

    references.append(
        {
            "id": source["id"],
            "file": f"images/{source['id']}",
            "patches": patches_file,
            "source": {
                "width": width,
                "height": height,
                "channels": channels,
            },
            "cls": digits(reference_cls),
        }
    )

(out_dir / "reference.json").write_text(
    json.dumps(
        {
            "resolution": resolution,
            "patch_grid": [grid_rows, grid_columns],
            "hidden_size": hidden_size,
            "export": str(export_dir),
            # What produced the numbers, so a later disagreement can be read
            # against the implementation it was measured on.
            "reference": {
                "processor": type(processor).__name__,
                "resample": int(processor.resample),
                "decoder": "torchvision.io.decode_image",
                "transformers": transformers.__version__,
                "torch": torch.__version__,
                "torchvision": torchvision.__version__,
                "num_threads": NUM_THREADS,
            },
            "images": references,
        },
        indent=2,
    )
)

print(f"dinov3 fixture at {resolution}px, patch grid {grid_rows}x{grid_columns}")
print(f"  {type(processor).__name__}, transformers {transformers.__version__}")
for entry in references:
    source = entry["source"]
    print(f"  {entry['id']:34s} {source['width']}x{source['height']} x{source['channels']}")
