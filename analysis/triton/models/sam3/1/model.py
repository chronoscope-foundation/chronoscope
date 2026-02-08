"""SAM3 segmentation model for Triton.

Loads SAM3 from HuggingFace and runs entity-prompted segmentation.
Returns detected regions as RLE-encoded masks.
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


# Entity prompts for automatic segmentation
ENTITY_PROMPTS = ["building", "bridge", "tower", "monument", "infrastructure"]

# Confidence threshold and limits
MIN_CONFIDENCE = 0.5
MAX_REGIONS = 20


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


# NOTE: Duplicated in analysis/1/model.py. Triton's Python backend loads
# each model in isolation, so there's no clean way to share code between
# models without complicating the deployment structure.
def compute_iou(mask1: np.ndarray, mask2: np.ndarray) -> float:
    """Compute Intersection over Union between two masks."""
    intersection = np.logical_and(mask1, mask2).sum()
    union = np.logical_or(mask1, mask2).sum()
    return intersection / union if union > 0 else 0.0


def compute_centroid_x(mask: np.ndarray) -> float:
    """Compute the x-coordinate of a mask's centroid."""
    ys, xs = np.where(mask)
    if len(xs) == 0:
        return 0.0
    return float(xs.mean())


def deduplicate_masks(
    masks: list[np.ndarray], scores: list[float], iou_threshold: float = 0.7
) -> list[tuple[np.ndarray, float]]:
    """Remove duplicate masks based on IoU threshold."""
    if not masks:
        return []

    # Sort by score descending
    sorted_pairs = sorted(
        zip(masks, scores, strict=True),
        key=lambda x: x[1],
        reverse=True,
    )

    keep: list[tuple[np.ndarray, float]] = []
    for mask, score in sorted_pairs:
        # Check if this mask overlaps too much with any kept mask
        is_duplicate = False
        for kept_mask, _ in keep:
            if compute_iou(mask, kept_mask) > iou_threshold:
                is_duplicate = True
                break

        if not is_duplicate:
            keep.append((mask, score))

    return keep


def sort_regions_left_to_right(
    regions: list[tuple[np.ndarray, float]],
) -> list[tuple[np.ndarray, float]]:
    """Sort regions by centroid x-coordinate (left to right).

    This makes region numbering predictable for the VLM - region 1 is leftmost.
    """
    return sorted(regions, key=lambda r: compute_centroid_x(r[0]))


def make_masks_exclusive(
    masks: list[tuple[np.ndarray, float]],
    survival_threshold: float = 0.1,
) -> list[tuple[np.ndarray, float]]:
    """Make masks mutually exclusive using confidence-based pixel claiming.

    Higher confidence masks claim pixels first. Masks that lose too many
    pixels (below survival_threshold of original) are removed.

    Args:
        masks: List of (mask, score) tuples, sorted by confidence descending.
        survival_threshold: Minimum fraction of original pixels a mask must retain.

    Returns:
        List of (modified_mask, score) tuples with non-overlapping masks.
    """
    if not masks:
        return []

    # Get image dimensions from first mask
    h, w = masks[0][0].shape
    claimed = np.zeros((h, w), dtype=bool)

    result = []
    for mask, score in masks:
        original_pixels = mask.sum()
        if original_pixels == 0:
            continue

        # Claim only unclaimed pixels
        exclusive_mask = mask & ~claimed
        remaining_pixels = exclusive_mask.sum()

        # Check survival threshold
        survival_ratio = remaining_pixels / original_pixels
        if survival_ratio >= survival_threshold:
            # Mark these pixels as claimed
            claimed |= exclusive_mask.astype(bool)
            result.append((exclusive_mask.astype(mask.dtype), score))

    return result


class TritonPythonModel:
    """SAM3 segmentation model."""

    def initialize(self, args):
        """Load SAM3 model from HuggingFace."""
        self.model_config = json.loads(args["model_config"])

        from sam3.model.sam3_image_processor import Sam3Processor
        from sam3.model_builder import build_sam3_image_model

        self.model = build_sam3_image_model()
        self.processor = Sam3Processor(self.model)
        pb_utils.Logger.log_info("Loaded SAM3 model")

    def execute(self, requests):
        """Process segmentation requests."""
        responses = []

        for request in requests:
            image_tensor = pb_utils.get_input_tensor_by_name(request, "image")
            image_b64 = get_string_from_tensor(image_tensor)

            # Check for optional prompts
            prompts_tensor = pb_utils.get_input_tensor_by_name(request, "prompts")
            prompts = None
            if prompts_tensor is not None:
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

    def _segment_with_sam3(
        self, image: Image.Image, prompts: list[str] | None = None
    ) -> list[dict]:
        """Run SAM3 segmentation with text prompts."""
        # Set the image once
        inference_state = self.processor.set_image(image)

        all_masks = []
        all_scores = []

        # Run with each entity prompt
        for prompt in prompts or ENTITY_PROMPTS:
            try:
                output = self.processor.set_text_prompt(state=inference_state, prompt=prompt)
                masks = output["masks"]
                scores = output["scores"]

                for mask, score in zip(masks, scores, strict=True):
                    if score >= MIN_CONFIDENCE:
                        # Convert mask to numpy if needed
                        if hasattr(mask, "cpu"):
                            mask = mask.cpu().numpy()
                        if mask.ndim == 3:
                            mask = mask.squeeze(0)
                        all_masks.append(mask > 0.5)
                        all_scores.append(float(score))

            except Exception as e:
                pb_utils.Logger.log_warn(f"Segmentation failed for prompt '{prompt}': {e}")

        # Deduplicate overlapping masks (IoU-based), then make exclusive (pixel-based)
        filtered = deduplicate_masks(all_masks, all_scores)
        filtered = make_masks_exclusive(filtered)
        # Limit count and sort left-to-right for predictable VLM numbering
        filtered = filtered[:MAX_REGIONS]
        filtered = sort_regions_left_to_right(filtered)

        # Convert to output format
        regions = [
            {"confidence": score, "mask": encode_rle(mask.astype(np.uint8))}
            for mask, score in filtered
        ]

        return regions

    def finalize(self):
        """Clean up."""
        pass
