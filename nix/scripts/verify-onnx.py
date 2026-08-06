"""Load every exported ONNX model and print its signature.

Run inside the export derivation. `torch.onnx.export` exiting 0 says nothing
about whether the artifacts are loadable: tensors past the 2 GB protobuf limit
spill into sibling files named after graph nodes, and node numbering restarts
per export, so exports sharing a directory overwrite each other's weights. The
model exported last still loads while an earlier one is quietly corrupt. This
turns that class of failure into a build failure.

Usage: verify-onnx.py <dir> [expected-model-name ...]
"""

import sys
from pathlib import Path

import onnxruntime as ort

root = Path(sys.argv[1])
expected = set(sys.argv[2:])

# CPU explicitly. ORT's default provider list puts CoreML first on darwin, and
# CoreML rejects any model using external data, which is every model here; the
# resulting error reads like corruption rather than a provider mismatch.
providers = ["CPUExecutionProvider"]

found = set()
failures = []

for sub in sorted(p for p in root.iterdir() if p.is_dir()):
    graphs = sorted(sub.glob("*.onnx"))
    if len(graphs) != 1:
        failures.append(f"{sub.name}: expected exactly one .onnx, found {len(graphs)}")
        continue
    try:
        sess = ort.InferenceSession(str(graphs[0]), providers=providers)
    except Exception as exc:  # noqa: BLE001 — any load failure is fatal here
        failures.append(f"{sub.name}: failed to load: {type(exc).__name__}: {exc}")
        continue

    found.add(sub.name)
    print(f"\n=== {sub.name} ({graphs[0].name}) ===")
    for spec in sess.get_inputs():
        print(f"  in   {spec.name:20s} {spec.type:16s} {spec.shape}")
    for spec in sess.get_outputs():
        print(f"  out  {spec.name:20s} {spec.type:16s} {spec.shape}")

missing = expected - found
if missing:
    failures.append(f"expected models never produced: {sorted(missing)}")

if failures:
    print("\nVERIFICATION FAILED", file=sys.stderr)
    for line in failures:
        print(f"  {line}", file=sys.stderr)
    sys.exit(1)

print(f"\nverified {len(found)} model(s): {sorted(found)}")
