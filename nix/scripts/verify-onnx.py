"""Load every exported ONNX model and check the baked constants fit it.

Run inside the export derivation. `torch.onnx.export` exiting 0 says nothing
about whether the artifacts are loadable: tensors past the 2 GB protobuf limit
spill into sibling files named after graph nodes, and node numbering restarts
per export, so exports sharing a directory overwrite each other's weights. The
model exported last still loads while an earlier one is quietly corrupt.

It also cross-checks the baked language constants against the decoder's own
declared inputs, since those constants replace a model that is deleted here and
nothing downstream would otherwise notice a shape or dtype mismatch until ORT
rejected the feed at runtime.

Usage: verify-onnx.py <dir> [expected-model-name ...]
"""

import json
import sys
from pathlib import Path

import onnxruntime as ort

# ONNX spells its types differently from numpy; compare through this.
ONNX_TO_NUMPY = {
    "tensor(bool)": "bool",
    "tensor(float)": "float32",
    "tensor(double)": "float64",
    "tensor(int64)": "int64",
    "tensor(int32)": "int32",
    "tensor(uint8)": "uint8",
}

root = Path(sys.argv[1])
expected = set(sys.argv[2:])

# CPU explicitly. ORT's default provider list puts CoreML first on darwin, and
# CoreML rejects any model using external data, which is every model here; the
# resulting error reads like corruption rather than a provider mismatch.
providers = ["CPUExecutionProvider"]

sessions = {}
failures = []

for name in sorted(expected):
    sub = root / name
    if not sub.is_dir():
        failures.append(f"{name}: expected model directory is missing")
        continue
    graphs = sorted(sub.glob("*.onnx"))
    if len(graphs) != 1:
        failures.append(f"{name}: expected exactly one .onnx, found {len(graphs)}")
        continue
    try:
        sessions[name] = ort.InferenceSession(str(graphs[0]), providers=providers)
    except Exception as exc:  # noqa: BLE001 — any load failure is fatal here
        failures.append(f"{name}: failed to load: {type(exc).__name__}: {exc}")
        continue

    print(f"\n=== {name} ({graphs[0].name}) ===")
    for spec in sessions[name].get_inputs():
        print(f"  in   {spec.name:20s} {spec.type:16s} {spec.shape}")
    for spec in sessions[name].get_outputs():
        print(f"  out  {spec.name:20s} {spec.type:16s} {spec.shape}")

# Anything left over is an export we did not expect and do not describe.
produced = {p.name for p in root.iterdir() if p.is_dir()}
for unexpected in sorted(produced - expected - {"language_constants"}):
    failures.append(f"{unexpected}: unexpected directory in the export output")

# The derivation always bakes, so a missing directory means the bake step
# silently did nothing.
constants_dir = root / "language_constants"
described = None
if not constants_dir.is_dir():
    failures.append("language_constants/: missing; the bake step did not run")
else:
    manifest = constants_dir / "language_constants.json"
    try:
        described = json.loads(manifest.read_text())
    except (OSError, ValueError) as exc:
        failures.append(f"language_constants.json: unreadable: {exc}")

if described is not None:
    decoder = sessions.get("decoder")
    if decoder is None:
        failures.append("language_constants present but the decoder did not load")
    else:
        decoder_inputs = {spec.name: spec for spec in decoder.get_inputs()}

        # `language_embeds` is a parameter of SAM3Decoder.forward and an entry
        # in its input_names, absent from the graph only because
        # _forward_grounding never reads it and the tracer pruned it. Should it
        # become live, the encoder that could supply it is already deleted.
        for name in sorted(decoder_inputs):
            if name.startswith("language_") and name not in described["tensors"]:
                failures.append(
                    f"{name}: decoder declares it but nothing baked it; the "
                    "language encoder is deleted, so it cannot be produced later"
                )

        print(f"\n=== language_constants (prompt {described['prompt']!r}) ===")
        for input_name, spec in described["tensors"].items():
            target = decoder_inputs.get(input_name)
            if target is None:
                failures.append(f"{input_name}: no such decoder input")
                continue
            want_dtype = ONNX_TO_NUMPY.get(target.type)
            if want_dtype is None:
                failures.append(
                    f"{input_name}: decoder wants {target.type}, which this "
                    "script has no numpy equivalent for; add it to ONNX_TO_NUMPY"
                )
            elif spec["dtype"] != want_dtype:
                failures.append(
                    f"{input_name}: baked {spec['dtype']}, decoder wants {want_dtype}"
                )
            if spec["shape"] != list(target.shape):
                failures.append(
                    f"{input_name}: baked {spec['shape']}, decoder wants {target.shape}"
                )
            blob = constants_dir / spec["file"]
            if not blob.is_file():
                failures.append(f"{input_name}: {spec['file']} is missing")
            elif blob.stat().st_size != spec["bytes"]:
                failures.append(
                    f"{input_name}: {spec['file']} is {blob.stat().st_size} bytes, "
                    f"described as {spec['bytes']}"
                )
            else:
                print(f"  {input_name:20s} {spec['dtype']:8s} {spec['shape']} ok")

if failures:
    print("\nVERIFICATION FAILED", file=sys.stderr)
    for line in failures:
        print(f"  {line}", file=sys.stderr)
    sys.exit(1)

print(f"\nverified {len(sessions)} model(s): {sorted(sessions)}")
