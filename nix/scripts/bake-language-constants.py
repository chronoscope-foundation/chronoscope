"""Bake SAM 3's box-only language conditioning into constants.

SAM 3's decoder always takes language features. Upstream's own box-only path
does not pass zeros for them: it runs the literal string "visual" through the
text encoder, commented there as needed "for the model to rely only on the
geometric prompt". It is a trained sentinel, not an absence, and substituting
zeros measurably changes both masks and per-query scores.

But the encoder is a pure function of that one fixed prompt, and the decoder
reads only two of its three outputs. So run it once here and keep the ~32 KB of
tensors, which lets a 1.3 GB model be dropped from the artifact set entirely.

The values come from the ONNX encoder, matching the graph semantics of the
decoder they feed. (Both models are loaded here, so the PyTorch encoder is
equally reachable; it is used below only as an independent reference.)

Usage: bake-language-constants.py <export-dir>
"""

import json
import shutil
import sys
from pathlib import Path

import numpy as np
import onnxruntime as ort
import pkg_resources
import torch

from sam3.model.sam3_image_processor import Sam3Processor
from sam3.model.tokenizer_ve import SimpleTokenizer
from sam3.model_builder import build_sam3_image_model

# ONNX and PyTorch differ slightly in float32 accumulation; a tokenization
# mismatch would not be slight. Observed drift on the reference run is 2.7e-05.
FEATURE_TOLERANCE = 1e-4

# Upstream's sentinel for "geometric prompt only", a bare literal in
# Sam3Processor.add_geometric_prompt (sam3/model/sam3_image_processor.py:141)
# with no named constant to import. The reference check below derives it
# independently by driving that branch, so a change upstream fails this build.
SENTINEL = "visual"

# Which decoder input each encoder output feeds. The encoder's third output,
# text_embeds, has no consumer anywhere in sam3 and is not a decoder input.
OUTPUTS = {
    "text_attention_mask": "language_mask",
    "text_memory": "language_features",
}

export_dir = Path(sys.argv[1])
lang_dir = export_dir / "language_encoder"
graphs = list(lang_dir.glob("*.onnx"))
if len(graphs) != 1:
    sys.exit(f"expected one language encoder graph in {lang_dir}, found {len(graphs)}")

session = ort.InferenceSession(str(graphs[0]), providers=["CPUExecutionProvider"])

(token_spec,) = session.get_inputs()
if len(token_spec.shape) != 2:
    sys.exit(f"expected a rank-2 token input, got {token_spec.shape}")
_batch, context_length = token_spec.shape
if _batch != 1:
    sys.exit(f"expected batch-1 token input, got {token_spec.shape}")
# ORT reports a dynamic dimension as its symbolic name, which would otherwise
# reach the tokenizer as a string and fail deep inside it.
if not isinstance(context_length, int):
    sys.exit(
        f"token input has a dynamic sequence dimension ({context_length!r}); "
        "the baked constants need a fixed context length"
    )

bpe_path = pkg_resources.resource_filename(
    "sam3", "assets/bpe_simple_vocab_16e6.txt.gz"
)
tokenizer = SimpleTokenizer(bpe_path=bpe_path)
tokens = tokenizer([SENTINEL], context_length=context_length).numpy().astype(np.int64)

produced = session.run(list(OUTPUTS), {token_spec.name: tokens})
baked = dict(zip(OUTPUTS.values(), produced))

# Tokenizing here rather than inside the model means a future sam3 could change
# its tokenization -- or its sentinel -- and leave these correctly shaped but
# semantically wrong, which no structural check can see.
#
# So the reference is taken by driving `add_geometric_prompt` on a state with no
# text prompt, which is precisely the branch that supplies the sentinel. Nothing
# below names it, so this compares against whatever upstream actually uses. The
# outputs it leaves in `backbone_out` are already named `language_mask` and
# `language_features`, which is where the mapping above comes from.
reference = build_sam3_image_model(device="cpu")
processor = Sam3Processor(reference, device="cpu")
with torch.inference_mode():
    # The image only has to exist: the sentinel comes from the prompt branch and
    # nothing below reads a pixel.
    state = processor.set_image(torch.zeros(3, 64, 64, dtype=torch.uint8))
    processor.add_geometric_prompt([0.5, 0.5, 0.5, 0.5], True, state)
expected = state["backbone_out"]

mask_drift = not np.array_equal(
    baked["language_mask"], np.asarray(expected["language_mask"].cpu()).astype(bool)
)
if mask_drift:
    sys.exit(
        f"baked language_mask (from {SENTINEL!r}) differs from the one "
        "add_geometric_prompt produced; upstream's sentinel may have changed"
    )

feature_drift = np.abs(
    baked["language_features"]
    - np.asarray(expected["language_features"].cpu(), dtype=np.float32)
).max()
if feature_drift > FEATURE_TOLERANCE:
    sys.exit(
        f"baked language_features (from {SENTINEL!r}) drift {feature_drift:.3e} "
        f"exceeds {FEATURE_TOLERANCE:.0e}; upstream's sentinel, tokenization, or "
        "encoder has changed"
    )
print(
    f"language constants agree with add_geometric_prompt (drift {feature_drift:.3e})"
)

out_dir = export_dir / "language_constants"
out_dir.mkdir(parents=True, exist_ok=True)

described = {}
for (source, decoder_input), array in zip(OUTPUTS.items(), produced):
    array = np.ascontiguousarray(array)
    (out_dir / f"{decoder_input}.bin").write_bytes(array.tobytes())
    described[decoder_input] = {
        "file": f"{decoder_input}.bin",
        "from_encoder_output": source,
        "dtype": str(array.dtype),
        "shape": list(array.shape),
        "bytes": array.nbytes,
    }

(out_dir / "language_constants.json").write_text(
    json.dumps(
        {
            "prompt": SENTINEL,
            "context_length": context_length,
            "tensors": described,
        },
        indent=2,
    )
)

# The encoder existed only to produce the above. Dropping it is the point.
shutil.rmtree(lang_dir)

total = sum(t["bytes"] for t in described.values())
print(f"baked {SENTINEL!r} language constants: {total} bytes across {len(described)}")
for name, spec in described.items():
    print(f"  {name:20s} {spec['dtype']:8s} {spec['shape']}")
