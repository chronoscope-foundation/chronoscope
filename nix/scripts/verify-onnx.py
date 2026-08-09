"""Verify an ONNX export and write the manifest the crate loads against.

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

CONSTANTS = "language_constants"

root = Path(sys.argv[1])
# Callers name everything the export should contain. `language_constants` is a
# directory of baked tensors rather than a graph, so it is checked differently.
expected = set(sys.argv[2:])
expect_constants = CONSTANTS in expected
expected.discard(CONSTANTS)

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

    # A sidecar carries what a signature cannot state. Every fact in one that the
    # graph also declares must be listed in `graph_assertions`, naming the tensor
    # it came from and what it reads off it — a shape `axis`, or `dtype` for the
    # element type — and the two are held to each other here.
    # `graph_assertions: []` is the writer's claim that the sidecar restates
    # nothing.
    sidecar = sub / f"{name}.json"
    if not sidecar.is_file():
        failures.append(
            f"{name}.json is missing; every model states what its signature "
            "cannot, even if that is only an empty assertion list"
        )
        continue
    try:
        claims = json.loads(sidecar.read_text())
    except ValueError as exc:
        failures.append(f"{name}.json: unreadable: {exc}")
        continue
    if not isinstance(claims.get("graph_assertions"), list):
        failures.append(
            f"{name}.json needs a graph_assertions list; state [] if it "
            "restates nothing the graph declares"
        )
        continue

    by_name = {s.name: s for s in sessions[name].get_inputs()}
    by_name.update({s.name: s for s in sessions[name].get_outputs()})
    for assertion in claims["graph_assertions"]:
        if not isinstance(assertion, dict) or not {
            "claim",
            "tensor",
        } <= assertion.keys():
            failures.append(
                f"{name}.json: malformed assertion {assertion!r}; needs "
                "claim, tensor, and one of axis or dtype"
            )
            continue
        claim, tensor = assertion["claim"], assertion["tensor"]
        if tensor not in by_name:
            failures.append(f"{name}.json asserts against absent tensor {tensor!r}")
            continue
        if claim not in claims:
            failures.append(f"{name}.json asserts {claim!r}, which it does not state")
            continue
        spec = by_name[tensor]
        if assertion.get("dtype"):
            declared = ONNX_TO_NUMPY.get(spec.type)
            if declared is None:
                failures.append(
                    f"{name}.json: {tensor} is {spec.type}, which this script "
                    "has no numpy name for; add it to ONNX_TO_NUMPY"
                )
            elif claims[claim] != declared:
                failures.append(
                    f"{name}.json claims {claim}={claims[claim]!r}, "
                    f"{tensor} is {declared}"
                )
            continue
        if "axis" not in assertion:
            failures.append(
                f"{name}.json: assertion {assertion!r} reads neither an axis "
                f"nor a dtype off {tensor}"
            )
            continue
        axis, shape = assertion["axis"], spec.shape
        if not -len(shape) <= axis < len(shape):
            failures.append(
                f"{name}.json: axis {axis} is out of range for {tensor} {shape}"
            )
            continue
        if claims[claim] != shape[axis]:
            failures.append(
                f"{name}.json claims {claim}={claims[claim]}, "
                f"{tensor}{shape} axis {axis} says {shape[axis]}"
            )
    if claims["graph_assertions"]:
        agreed = [a["claim"] for a in claims["graph_assertions"] if isinstance(a, dict)]
        print(f"  sidecar agrees on {agreed}")

# Anything left over is an export we did not expect and do not describe. Scoped
# to directories because one directory per model is the layout being checked;
# the root-level files are this script's own manifest and whatever a downstream
# derivation lays beside it, and widening the sweep would reject both.
produced = {p.name for p in root.iterdir() if p.is_dir()}
for unexpected in sorted(produced - expected - {CONSTANTS}):
    failures.append(f"{unexpected}: unexpected directory in the export output")

constants_dir = root / CONSTANTS
described = None
if expect_constants:
    if not constants_dir.is_dir():
        failures.append(f"{CONSTANTS}/: missing; the bake step did not run")
    else:
        manifest = constants_dir / "language_constants.json"
        try:
            described = json.loads(manifest.read_text())
        except (OSError, ValueError) as exc:
            failures.append(f"language_constants.json: unreadable: {exc}")
elif constants_dir.is_dir():
    failures.append(f"{CONSTANTS}/: present but the caller did not expect it")

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

# One manifest per export, written only once everything above agrees, so the
# crate never reads a description of an artifact that failed its own checks.
# It carries the graph signatures plus the facts a signature cannot state: the
# baked score threshold, the baked preprocessing, the patch grid, which
# constants feed which decoder input.
manifest = {"models": {}}
for name, session in sorted(sessions.items()):
    sub = root / name
    (graph,) = sub.glob("*.onnx")
    entry = {
        "graph": str(graph.relative_to(root)),
        "inputs": [
            {"name": s.name, "dtype": s.type, "shape": s.shape}
            for s in session.get_inputs()
        ],
        "outputs": [
            {"name": s.name, "dtype": s.type, "shape": s.shape}
            for s in session.get_outputs()
        ],
    }
    sidecar = sub / f"{name}.json"
    if sidecar.is_file():
        entry["metadata"] = json.loads(sidecar.read_text())
    manifest["models"][name] = entry

if described is not None:
    manifest["language_constants"] = {
        "directory": CONSTANTS,
        **described,
    }
    # Recorded because the encoder's remaining output is unreachable: the
    # decoder never declared it and the model that produced it is gone.
    manifest["language_constants"]["encoder_deleted"] = True

(root / "manifest.json").write_text(json.dumps(manifest, indent=2))

print(f"\nverified {len(sessions)} model(s): {sorted(sessions)}")
print(f"wrote manifest.json describing {sorted(manifest['models'])}")
