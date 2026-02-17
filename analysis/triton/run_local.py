#!/usr/bin/env python3
"""Batch pipeline runner for local corpus testing.

Reads newline-delimited image paths from stdin, runs each through the
SAM3 + DINOv3 pipeline (VLM skipped), and writes newline-delimited JSON
to stdout. Progress and errors go to stderr.

Usage:
    echo "/path/to/image.jpg" | python run_local.py
"""

import base64
import json
import os
import sys
import time
import traceback

import numpy as np

import mock_triton
from model_loader import load_triton_model, register_pipeline, vlm_skip_handler


def main() -> None:
    """Run the pipeline on images from stdin."""
    os.environ.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")

    # Read image paths from stdin.
    paths = [line.strip() for line in sys.stdin if line.strip()]
    if not paths:
        print("No image paths provided on stdin", file=sys.stderr)
        sys.exit(1)

    print("Loading models...", file=sys.stderr)
    load_start = time.time()

    # Load models once (session-scoped).
    sam3_model = load_triton_model("sam3")
    dinov3_model = load_triton_model("dinov3")

    load_elapsed = time.time() - load_start
    print(f"Models loaded in {load_elapsed:.1f}s", file=sys.stderr)

    # Register models and get BLS orchestrator.
    mock_triton.clear_models()
    pipeline = register_pipeline(sam3_model, dinov3_model, vlm_skip_handler)

    total = len(paths)
    for i, image_path in enumerate(paths, 1):
        print(f"[{i}/{total}] {image_path}", file=sys.stderr)
        start = time.time()

        try:
            with open(image_path, "rb") as f:
                image_bytes = f.read()
            image_b64 = base64.b64encode(image_bytes)

            request = mock_triton.InferenceRequest(
                model_name="analysis",
                requested_output_names=["result"],
                inputs=[
                    mock_triton.Tensor("image", np.array([[image_b64]])),
                    mock_triton.Tensor("schema", np.array([[b"{}"]])),
                    mock_triton.Tensor("skip_vlm", np.array([[True]])),
                ],
            )

            responses = pipeline.execute([request])
            elapsed_ms = int((time.time() - start) * 1000)

            response = responses[0]
            if response.has_error():
                result = {
                    "outcome": "image_rejected",
                    "reason": f"Pipeline error: {response.error().message()}",
                }
            else:
                result_tensor = mock_triton.get_output_tensor_by_name(response, "result")
                result_json = result_tensor.as_numpy().flatten()[0].decode("utf-8")
                result = json.loads(result_json)

            output = {
                "path": image_path,
                "elapsed_ms": elapsed_ms,
                "result": result,
            }
            print(json.dumps(output), flush=True)

        except Exception as e:
            traceback.print_exc(file=sys.stderr)
            elapsed_ms = int((time.time() - start) * 1000)
            output = {
                "path": image_path,
                "elapsed_ms": elapsed_ms,
                "result": {
                    "outcome": "image_rejected",
                    "reason": f"Runner error: {e}",
                },
            }
            print(json.dumps(output), flush=True)

    print(f"Done: {total} images processed", file=sys.stderr)


if __name__ == "__main__":
    main()
