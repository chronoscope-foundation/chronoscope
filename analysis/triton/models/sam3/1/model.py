"""SAM3 segmentation model for Triton.

Loads SAM3 from HuggingFace and runs text-prompted segmentation.
Returns raw confidence-filtered regions as RLE-encoded masks.

Post-processing (deduplication, exclusive pixel claiming, sorting) is
intentionally left to callers — composite subimage detection and entity
detection have different requirements. See analysis/1/model.py.
"""

import base64
import io
import json
from typing import Any

import numpy as np
import triton_python_backend_utils as pb_utils
from PIL import Image


def get_string_from_tensor(tensor: Any) -> str:
    """Extract a UTF-8 string from a Triton tensor.

    Handles both 1D and 2D tensor shapes from HTTP vs BLS calls.

    NOTE: This helper is intentionally duplicated across model files. Triton's
    Python backend loads each model in isolation, so there's no clean way to
    share code between models without complicating the deployment structure.
    """
    result: str = tensor.as_numpy().flatten()[0].decode("utf-8")
    return result


# Minimum confidence for a mask to be included in results
MIN_CONFIDENCE = 0.5

# Threshold for binarizing soft probability masks from SAM3's decoder
MASK_BINARIZATION_THRESHOLD = 0.5


def encode_rle(mask: np.ndarray) -> list[int]:
    """Encode a binary mask as RLE (COCO-style, column-major order).

    Args:
        mask: Binary mask of shape (H, W) with values 0 or 1.

    Returns:
        List of run lengths (alternating background/foreground, starting with background).
    """
    # Flatten in column-major (Fortran) order for COCO compatibility
    flat = mask.flatten(order="F")

    counts: list[int] = []
    current_val = 0  # Start counting background
    run_length = 0

    for val in flat:
        if val == current_val:
            run_length += 1
        else:
            counts.append(run_length)
            current_val = val
            run_length = 1

    counts.append(run_length)

    return counts


def _select_device() -> str:
    """Select best available device: CUDA > MPS > CPU.

    NOTE: This helper is intentionally duplicated across model files. Triton's
    Python backend loads each model in isolation, so there's no clean way to
    share code between models without complicating the deployment structure.
    """
    import torch

    if torch.cuda.is_available():
        return "cuda"
    if hasattr(torch.backends, "mps") and torch.backends.mps.is_available():
        return "mps"
    return "cpu"


class TritonPythonModel:
    """SAM3 segmentation model."""

    def initialize(self, args):
        """Load SAM3 model on best available device."""
        self.model_config = json.loads(args["model_config"])

        from sam3.model.sam3_image_processor import Sam3Processor
        from sam3.model_builder import build_sam3_image_model

        device = _select_device()

        # Build on CPU first — SAM3 initialization has ops that fail on MPS.
        # Then .to(device) moves parameters and buffers, but compilable_cord_cache
        # and coord_cache are plain tuples (not registered buffers), so .to()
        # misses them. We move those manually to avoid device mismatch at inference.
        self.model = build_sam3_image_model(device="cpu")
        if device != "cpu":
            self.model = self.model.to(device)
            if device == "mps":
                for module in self.model.modules():
                    cache = getattr(module, "compilable_cord_cache", None)
                    if cache is not None:
                        h, w = cache
                        module.compilable_cord_cache = (h.to(device), w.to(device))
                    if hasattr(module, "coord_cache"):
                        for key in module.coord_cache:
                            h, w = module.coord_cache[key]
                            module.coord_cache[key] = (h.to(device), w.to(device))

        self.processor = Sam3Processor(
            self.model, device=device, confidence_threshold=MIN_CONFIDENCE
        )

        n_params = sum(p.numel() for p in self.model.parameters())
        pb_utils.Logger.log_info(f"Loaded SAM3: {n_params / 1e6:.0f}M params on {device}")

    def execute(self, requests):
        """Process segmentation requests."""
        responses = []

        for request in requests:
            image_tensor = pb_utils.get_input_tensor_by_name(request, "image")
            image_b64 = get_string_from_tensor(image_tensor)

            # Prompts are required — the caller decides what to segment
            prompts_tensor = pb_utils.get_input_tensor_by_name(request, "prompts")
            if prompts_tensor is None:
                raise RuntimeError("SAM3 requires 'prompts' input")
            prompts = [p.decode("utf-8") for p in prompts_tensor.as_numpy().flatten()]

            # Decode image
            image_bytes = base64.b64decode(image_b64)
            image = Image.open(io.BytesIO(image_bytes)).convert("RGB")

            # Run segmentation
            regions = self._segment_with_sam3(image, prompts=prompts)

            # Encode result
            result_json = json.dumps(regions)
            output_tensor = pb_utils.Tensor("regions", np.array([result_json.encode("utf-8")]))
            responses.append(pb_utils.InferenceResponse([output_tensor]))

        return responses

    def _segment_with_sam3(self, image: Image.Image, prompts: list[str]) -> list[dict]:
        """Run SAM3 segmentation with text prompts.

        Processes prompts sequentially via the public Sam3Processor API.
        The image backbone runs once (set_image) and is shared across all
        prompts; each set_text_prompt call runs the encoder/decoder/segmentation
        heads for one prompt at a time, keeping peak memory bounded.

        Returns raw confidence-filtered regions. Callers are responsible for
        any post-processing (deduplication, exclusive masks, sorting, limits).
        """

        # Image backbone runs once (shared across all prompts)
        state = self.processor.set_image(image)

        return self._segment_sequential(state, prompts)

    def _segment_sequential(self, state: dict, prompts: list[str]) -> list[dict]:
        """One forward pass per prompt via the public API."""
        results: list[dict] = []

        for prompt in prompts:
            try:
                output = self.processor.set_text_prompt(state=state, prompt=prompt)
                masks = output["masks"]
                scores = output["scores"]

                for mask, score in zip(masks, scores, strict=True):
                    if hasattr(mask, "cpu"):
                        mask = mask.cpu().numpy()
                    if mask.ndim == 3:
                        mask = mask.squeeze(0)
                    results.append(
                        {
                            "confidence": float(score),
                            "mask": encode_rle(
                                (mask > MASK_BINARIZATION_THRESHOLD).astype(np.uint8)
                            ),
                            "prompt": prompt,
                        }
                    )
            except Exception as e:
                pb_utils.Logger.log_warn(f"Segmentation failed for prompt '{prompt}': {e}")

        return results

    def finalize(self):
        """Clean up."""
        pass
