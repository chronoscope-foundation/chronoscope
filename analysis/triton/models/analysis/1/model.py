"""BLS orchestration model for the subimage-centric analysis pipeline.

Orchestrates per-subimage: SAM3 segmentation → image annotation → VLM analysis → DINOv3 embeddings.

Output shape: AnalysisResult { subimages: [{ bounds, analysis }] }
"""

import base64
import io
import json
import os
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Any, NamedTuple

import numpy as np
import triton_python_backend_utils as pb_utils
from PIL import Image, ImageDraw, ImageFont

# Import from same directory (needed for both Triton and test environments)
sys.path.insert(0, os.path.dirname(__file__))
from baml_converter import jsonschema_to_baml

# Kelly's 22 colors of maximum contrast (1965), minus white and black.
# Colors are ordered for maximum distinguishability; we cycle if >20 regions.
# https://gist.github.com/ollieglass/f6ddd781eeae1d24e391265432297538
REGION_COLORS = [
    (243, 195, 0),  # Yellow
    (135, 86, 146),  # Purple
    (243, 132, 0),  # Orange
    (161, 202, 241),  # Light blue
    (190, 0, 50),  # Red
    (194, 178, 128),  # Buff
    (132, 132, 130),  # Gray
    (0, 136, 86),  # Green
    (230, 143, 172),  # Pink
    (0, 103, 165),  # Blue
    (249, 147, 121),  # Apricot
    (96, 78, 151),  # Violet
    (246, 166, 0),  # Orange yellow
    (179, 68, 108),  # Purplish red
    (220, 211, 0),  # Greenish yellow
    (136, 45, 23),  # Brown
    (141, 182, 0),  # Yellow green
    (101, 69, 34),  # Brownish orange
    (226, 88, 34),  # Reddish orange
    (43, 61, 38),  # Olive green
]

MASK_ALPHA = int(0.3 * 255)
MAX_IMAGE_BYTES = 5 * 1024 * 1024  # 5MB limit to prevent OOM

# Max image dimension (longest edge) before sending to VLM.
# Qwen3-VL: 28x28 pixels = 1 token, max 16384 tokens/image.
# 2048x2048 = ~5.4K tokens × 2 images = ~11K tokens, well within 128K context.
VLM_MAX_IMAGE_DIM = 2048

# Max region crops to embed via DINOv3 (caps GPU memory usage).
# SAM3 outputs regions in left-to-right spatial order, not confidence order.
MAX_REGION_CROPS = 16

# Max entity regions per subimage (caps VLM complexity and DINOv3 budget).
MAX_ENTITY_REGIONS = 20

# Timeouts are generous placeholder values — no production latency data yet.
# VLM with 32K max_tokens on a 72B model can take minutes for complex images.
SAM3_TIMEOUT_MS = 120_000  # 2 min
VLM_TIMEOUT_MS = 300_000  # 5 min
DINOV3_TIMEOUT_MS = 60_000  # 1 min
# Safety margin for future.result() after as_completed returns it.
# The future should already be done; this just prevents indefinite hangs
# from edge cases in the executor.
FUTURE_RESULT_TIMEOUT_S = 10

# Subimage detection thresholds
SUBIMAGE_MIN_AREA_FRAC = 0.05  # Reject subimages <5% of image area
SUBIMAGE_MAX_AREA_FRAC = 0.95  # Reject subimages >95% of image area
SUBIMAGE_MIN_BBOX_FILL = 0.7  # Reject if mask/bbox area ratio < 0.7
SUBIMAGE_MAX_OVERLAP_IOU = 0.3  # Fallback to single if IoU > 0.3 between any pair
SUBIMAGE_CONTAINMENT_THRESHOLD = 0.7  # Remove container if smaller mask >70% contained

# Text prompt for composite image panel detection. Targets distinct image
# regions within collages, side-by-side comparisons, or multi-panel layouts.
SUBIMAGE_PROMPT = "separate photograph in a collage"


class DinoBatchEntry(NamedTuple):
    """Entry in the DINOv3 embedding batch."""

    subimage_idx: int
    entry_type: str  # "subimage" or "region"
    region_idx: int | None
    crop: Image.Image


def get_string_from_tensor(tensor: Any) -> str:
    """Extract a UTF-8 string from a Triton tensor.

    Handles both 1D and 2D tensor shapes from HTTP vs BLS calls.

    NOTE: This helper is intentionally duplicated across model files. Triton's
    Python backend loads each model in isolation, so there's no clean way to
    share code between models without complicating the deployment structure.
    """
    result: str = tensor.as_numpy().flatten()[0].decode("utf-8")
    return result


def decode_rle(counts: list[int], height: int, width: int) -> np.ndarray:
    """Decode RLE mask back to binary array.

    Args:
        counts: List of run lengths (alternating background/foreground).
        height: Mask height.
        width: Mask width.
    """
    mask = np.zeros(height * width, dtype=np.uint8)
    pos = 0
    is_fg = False  # First run is background

    for run_length in counts:
        if is_fg:
            mask[pos : pos + run_length] = 1
        pos += run_length
        is_fg = not is_fg

    return mask.reshape((height, width), order="F")


def compress_rle(counts: list[int]) -> str:
    """Compress RLE integer array to COCO compressed string format.

    Uses modified LEB128 encoding: 5 bits per chunk, +48 for ASCII, bit 5 = continuation.
    This is the same format used by pycocotools for compact wire transfer.
    """
    encoded = []
    for x in counts:
        if x == 0:
            encoded.append(48)  # '0'
        else:
            while x > 0:
                chunk = x & 0x1F  # Take 5 bits
                x >>= 5
                if x > 0:
                    chunk |= 0x20  # Set continuation bit
                encoded.append(chunk + 48)
    return bytes(encoded).decode("ascii")


def decompress_rle(counts_str: str) -> list[int]:
    """Decompress COCO compressed RLE string to integer counts."""
    counts = []
    x = 0
    shift = 0
    for c in counts_str:
        val = ord(c) - 48
        x |= (val & 0x1F) << shift
        if val & 0x20:
            shift += 5
        else:
            counts.append(x)
            x = 0
            shift = 0
    return counts


def make_full_image_mask(height: int, width: int) -> str:
    """Create an all-ones compressed RLE mask (full image, no masking)."""
    return compress_rle([0, height * width])


def mask_bbox(mask: np.ndarray) -> tuple[int, int, int, int] | None:
    """Compute bounding box of a binary mask. Returns (x, y, w, h) or None if empty."""
    ys, xs = np.where(mask == 1)
    if len(xs) == 0:
        return None
    x_min, x_max = int(xs.min()), int(xs.max())
    y_min, y_max = int(ys.min()), int(ys.max())
    return (x_min, y_min, x_max - x_min + 1, y_max - y_min + 1)


def compute_iou(mask1: np.ndarray, mask2: np.ndarray) -> float:
    """Compute Intersection over Union between two masks."""
    intersection = np.logical_and(mask1, mask2).sum()
    union = np.logical_or(mask1, mask2).sum()
    return float(intersection / union) if union > 0 else 0.0


def compute_containment(mask1: np.ndarray, mask2: np.ndarray) -> float:
    """Compute containment: intersection / area of the smaller mask.

    Measures how much of the smaller mask is contained within the larger.
    Returns 1.0 when the smaller mask is fully inside the larger, 0.0 when
    they don't overlap at all.

    This is distinct from IoU: two masks can have moderate IoU but high
    containment when a large region envelops a smaller one.
    """
    intersection = float(np.logical_and(mask1, mask2).sum())
    smaller_area = float(min(mask1.sum(), mask2.sum()))
    return intersection / smaller_area if smaller_area > 0 else 0.0


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


def sort_regions_left_to_right(
    regions: list[tuple[np.ndarray, float]],
) -> list[tuple[np.ndarray, float]]:
    """Sort regions by centroid x-coordinate (left to right).

    This makes region numbering predictable for the VLM - region 1 is leftmost.
    """
    return sorted(regions, key=lambda r: compute_centroid_x(r[0]))


def postprocess_entity_regions(raw_regions: list[dict], height: int, width: int) -> list[dict]:
    """Post-process raw SAM3 output for entity detection.

    Applies deduplication, exclusive pixel claiming, region limit, and
    left-to-right sorting. This pipeline is specific to entity/building
    detection — composite subimage detection uses different logic.
    """
    if not raw_regions:
        return []

    # Decode RLE masks back to numpy arrays
    masks = [decode_rle(r["mask"], height, width) for r in raw_regions]
    scores = [r["confidence"] for r in raw_regions]

    # Deduplicate overlapping masks (IoU-based), then make exclusive (pixel-based)
    filtered = deduplicate_masks(masks, scores)
    filtered = make_masks_exclusive(filtered)
    # Limit count and sort left-to-right for predictable VLM numbering
    filtered = filtered[:MAX_ENTITY_REGIONS]
    filtered = sort_regions_left_to_right(filtered)

    # Re-encode to RLE
    return [
        {"confidence": score, "mask": _encode_rle_mask(mask.astype(np.uint8))}
        for mask, score in filtered
    ]


def annotate_image(image: Image.Image, regions: list[dict[str, Any]]) -> Image.Image:
    """Create Set-of-Mark (SoM) annotated image.

    Overlays semi-transparent colored masks and numeric labels on each region.
    """
    # Convert to RGBA for alpha compositing
    annotated = image.convert("RGBA")
    overlay = Image.new("RGBA", annotated.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)

    # Scale font size based on image dimensions (larger images need larger labels)
    # Target: labels visible but not overwhelming (~1/25 of shortest edge)
    min_dim = min(image.size)
    font_size = max(32, min(96, min_dim // 25))

    # Try to load a decent font, fall back to default
    font: ImageFont.FreeTypeFont | ImageFont.ImageFont = ImageFont.load_default()
    for font_path in [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "DejaVuSans-Bold.ttf",
    ]:
        try:
            font = ImageFont.truetype(font_path, font_size)
            break
        except OSError:
            continue

    for idx, region in enumerate(regions):
        color = REGION_COLORS[idx % len(REGION_COLORS)]

        # Decode mask from RLE counts
        mask = decode_rle(region["mask"], image.height, image.width)

        # Create colored mask overlay
        mask_rgba = np.zeros((*mask.shape, 4), dtype=np.uint8)
        mask_rgba[mask == 1] = (*color, MASK_ALPHA)
        mask_image = Image.fromarray(mask_rgba, mode="RGBA")
        overlay = Image.alpha_composite(overlay, mask_image)

        # Find centroid for label placement
        ys, xs = np.where(mask == 1)
        if len(xs) > 0:
            cx, cy = int(xs.mean()), int(ys.mean())

            # Draw label background and text
            label = str(idx)
            bbox = font.getbbox(label)
            text_left, text_top, text_right, text_bottom = bbox
            tw = text_right - text_left
            th = text_bottom - text_top

            # Draw circle background (padding scales with font size)
            padding = font_size // 3
            radius = max(tw, th) // 2 + padding
            draw = ImageDraw.Draw(overlay)
            draw.ellipse(
                [cx - radius, cy - radius, cx + radius, cy + radius],
                fill=(*color, 200),
                outline=(255, 255, 255, 255),
                width=2,
            )
            # Center text properly, accounting for font baseline offset
            draw.text(
                (cx - tw // 2 - text_left, cy - th // 2 - text_top),
                label,
                fill=(0, 0, 0, 255),
                font=font,
            )

    # Composite and convert back to RGB
    result = Image.alpha_composite(annotated, overlay)
    return result.convert("RGB")


def build_vlm_prompt(schema_json: str) -> str:
    """Build the VLM prompt for subimage analysis.

    Uses BAML-style type definitions for concise, LLM-friendly schema representation.
    The actual JSON Schema is still used for constrained decoding.

    This prompt is the cacheable prefix - region-specific rules come after the images.
    """
    schema = json.loads(schema_json)
    baml_schema = jsonschema_to_baml(schema)

    return f"""\
You are analyzing images for a historical built environment research project
cataloging buildings, bridges, infrastructure, and other structures.

Your response must be valid JSON matching this schema:

{baml_schema}

DECISION: If the image shows built structures (buildings, bridges, towers, monuments,
infrastructure), respond with status "analyzed". If not relevant (portraits, animals,
food, memes, pure landscapes with no structures), respond with status "rejected" and
a brief reason.

For "analyzed" responses:
- You will see two images showing exactly the same scene:
  1. The original photograph - use this to see fine details clearly
  2. The annotated photograph - same image with colored overlays marking detected
     regions, each labeled with a circled number
- Find region numbers and boundaries in the ANNOTATED image
  (look for numbers like 1, 2, 3 inside colored circles)
- Examine the corresponding area in the ORIGINAL image for architectural
  details, text, and features
- The annotations may partially obscure details, so always cross-reference
  with the original"""


class TritonPythonModel:
    """BLS orchestration model for the subimage-centric analysis pipeline."""

    def initialize(self, args):
        """Initialize the orchestrator."""
        self.model_config = json.loads(args["model_config"])

        # Model IDs for provenance tracking
        self.vlm_model_name = "unknown"
        vlm_model_json = os.path.join(
            args.get("model_repository", "/models"),
            "vlm",
            "1",
            "model.json",
        )
        if os.path.exists(vlm_model_json):
            with open(vlm_model_json) as f:
                vlm_config = json.load(f)
            self.vlm_model_name = vlm_config.get("model", "unknown")

        self.sam3_model_name = "facebook/sam2.1-hiera-large"
        self.dinov3_model_name = "facebook/dinov3-vitl16-pretrain-lvd1689m"
        self.git_revision = os.environ.get("ANALYSIS_GIT_SHA", "unknown")

        self.versions = {
            "vlm": self.vlm_model_name,
            "sam3": self.sam3_model_name,
            "dinov3": self.dinov3_model_name,
            "git_sha": self.git_revision,
        }

        pb_utils.Logger.log_info("Analysis BLS model initialized")

    def execute(self, requests):
        """Process analysis requests through the subimage pipeline.

        Pipeline: detect subimages → per-subimage (SAM3 → annotate → VLM)
        → batched DINOv3 → assemble.
        """
        responses = []

        for request in requests:
            image_tensor = pb_utils.get_input_tensor_by_name(request, "image")
            schema_tensor = pb_utils.get_input_tensor_by_name(request, "schema")

            image_b64 = get_string_from_tensor(image_tensor)
            schema_json = get_string_from_tensor(schema_tensor)

            # Validate image size to prevent OOM
            estimated_size = len(image_b64) * 3 // 4
            if estimated_size > MAX_IMAGE_BYTES:
                size_mb = estimated_size // (1024 * 1024)
                limit_mb = MAX_IMAGE_BYTES // (1024 * 1024)
                result = {
                    "subimages": [
                        {
                            "bounds": _full_image_bounds(1, 1),
                            "analysis": {
                                "status": "rejected",
                                "reason": (
                                    f"Image too large: {size_mb}MB exceeds {limit_mb}MB limit"
                                ),
                            },
                        }
                    ],
                    "versions": self.versions,
                }
                responses.append(_make_response(result))
                continue

            # Decode image
            image_bytes = base64.b64decode(image_b64)
            original = Image.open(io.BytesIO(image_bytes)).convert("RGB")

            # Step 1: Detect subimages (panels) in the source image
            subimage_bounds = self._detect_subimages(image_b64, original)

            # Step 2: Per-subimage analysis
            subimage_results = []
            dino_batch: list[DinoBatchEntry] = []

            # Pre-compute prompt once
            vlm_prompt = build_vlm_prompt(schema_json)

            # Phase 1: Fire all SAM3 requests concurrently
            sam_futures = []
            for si_idx, bounds in enumerate(subimage_bounds):
                bbox = bounds["bbox"]
                crop = original.crop(
                    (
                        bbox["x"],
                        bbox["y"],
                        bbox["x"] + bbox["width"],
                        bbox["y"] + bbox["height"],
                    )
                )
                crop_b64 = self._encode_image(crop, max_dim=None)
                sam_request = pb_utils.InferenceRequest(
                    model_name="sam3",
                    requested_output_names=["regions"],
                    inputs=[pb_utils.Tensor("image", np.array([[crop_b64.encode("utf-8")]]))],
                )
                sam_request.set_timeout_ms(SAM3_TIMEOUT_MS)
                sam_futures.append((si_idx, bounds, crop, sam_request.async_exec()))

            # Phase 2: Collect SAM3 results, prepare VLM inputs
            vlm_inputs = []
            for si_idx, bounds, crop, sam_future in sam_futures:
                sam_response = sam_future.get()
                if sam_response.has_error():
                    raise RuntimeError(
                        f"SAM3 error on subimage {si_idx}: {sam_response.error().message()}"
                    )

                regions_tensor = pb_utils.get_output_tensor_by_name(sam_response, "regions")
                regions_json = get_string_from_tensor(regions_tensor)
                raw_regions = json.loads(regions_json)

                # Post-process: dedup, exclusive pixel claiming, sort, limit
                regions = postprocess_entity_regions(raw_regions, crop.height, crop.width)

                annotated = annotate_image(crop, regions)
                original_b64 = self._encode_image(crop)
                annotated_b64 = self._encode_image(annotated)

                compressed_regions = [
                    {**r, "mask": {"counts": compress_rle(r["mask"])}} for r in regions
                ]

                vlm_inputs.append(
                    (
                        si_idx,
                        bounds,
                        crop,
                        compressed_regions,
                        original_b64,
                        annotated_b64,
                        len(regions),
                    )
                )

            # Phase 3: Run VLM concurrently across subimages, then queue DINOv3
            vlm_results: dict[int, dict[str, Any]] = {}
            with ThreadPoolExecutor(max_workers=len(vlm_inputs) or 1) as pool:
                future_to_idx = {
                    pool.submit(
                        self._call_vlm,
                        vlm_prompt,
                        orig_b64,
                        ann_b64,
                        schema_json,
                        n_regions,
                    ): si_idx
                    for si_idx, _, _, _, orig_b64, ann_b64, n_regions in vlm_inputs
                }
                for future in as_completed(future_to_idx, timeout=VLM_TIMEOUT_MS / 1000 + 30):
                    idx = future_to_idx[future]
                    try:
                        vlm_results[idx] = future.result(timeout=FUTURE_RESULT_TIMEOUT_S)
                    except (json.JSONDecodeError, KeyError, TimeoutError) as e:
                        vlm_results[idx] = {"error": f"VLM call failed: {e}"}

            for si_idx, bounds, crop, compressed_regions, _, _, _ in vlm_inputs:
                vlm_result = vlm_results[si_idx]

                subimage_results.append(
                    {
                        "bounds": bounds,
                        "vlm": vlm_result,
                        "compressed_regions": compressed_regions,
                    }
                )

                # Queue DINOv3 crops for analyzed subimages
                if vlm_result.get("status") == "analyzed":
                    dino_batch.append(DinoBatchEntry(si_idx, "subimage", None, crop))
                    for ri, region in enumerate(compressed_regions[:MAX_REGION_CROPS]):
                        region_crop = self._crop_region_bbox(crop, region["mask"], padding=0.05)
                        dino_batch.append(DinoBatchEntry(si_idx, "region", ri, region_crop))

            # Step 3: Batched DINOv3 embeddings
            embeddings_map: dict[tuple[int, str, int | None], list[float] | None] = {}
            if dino_batch:
                dino_images = [entry.crop for entry in dino_batch]
                embeddings = self._call_dinov3(dino_images)
                for i, entry in enumerate(dino_batch):
                    key = (entry.subimage_idx, entry.entry_type, entry.region_idx)
                    embeddings_map[key] = embeddings[i]

            # Step 4: Assemble final AnalysisResult
            subimages = []
            for si_idx, si_data in enumerate(subimage_results):
                vlm = si_data["vlm"]
                bounds = si_data["bounds"]
                compressed_regions = si_data["compressed_regions"]

                if vlm.get("status") == "analyzed":
                    subimage_embedding = embeddings_map.get((si_idx, "subimage", None))
                    analysis = self._assemble_analyzed(
                        vlm, compressed_regions, si_idx, embeddings_map, subimage_embedding
                    )
                elif vlm.get("status") == "rejected":
                    analysis = {"status": "rejected", "reason": vlm.get("reason", "Unknown")}
                elif "error" in vlm:
                    analysis = {"status": "error", "message": vlm["error"]}
                else:
                    analysis = {"status": "error", "message": "Unexpected VLM output format"}

                subimages.append({"bounds": bounds, "analysis": analysis})

            result = {
                "subimages": subimages,
                "versions": self.versions,
            }
            responses.append(_make_response(result))

        return responses

    def _detect_subimages(self, image_b64: str, image: Image.Image) -> list[dict[str, Any]]:
        """Detect subimage panels using SAM3 with image-level prompts.

        Returns list of SubimageBounds dicts. Falls back to a single full-image
        bounds if detection finds 0-1 panels, fails, or panels overlap too much.
        """
        full_bounds = _full_image_bounds(image.width, image.height)

        # Call SAM3 with subimage detection prompt
        prompts_array = np.array([[SUBIMAGE_PROMPT.encode("utf-8")]])
        sam_request = pb_utils.InferenceRequest(
            model_name="sam3",
            requested_output_names=["regions"],
            inputs=[
                pb_utils.Tensor("image", np.array([[image_b64.encode("utf-8")]])),
                pb_utils.Tensor("prompts", prompts_array),
            ],
        )
        sam_request.set_timeout_ms(SAM3_TIMEOUT_MS)
        sam_response = sam_request.exec()

        if sam_response.has_error():
            raise RuntimeError(f"Subimage SAM3 error: {sam_response.error().message()}")

        regions_tensor = pb_utils.get_output_tensor_by_name(sam_response, "regions")
        regions_json = get_string_from_tensor(regions_tensor)
        raw_regions = json.loads(regions_json)

        if not raw_regions:
            return [full_bounds]

        # Filter by area and bbox fill ratio
        image_area = image.width * image.height
        usable = []
        for region in raw_regions:
            mask = decode_rle(region["mask"], image.height, image.width)
            area = int(mask.sum())
            area_frac = area / image_area

            if area_frac < SUBIMAGE_MIN_AREA_FRAC or area_frac > SUBIMAGE_MAX_AREA_FRAC:
                continue

            bbox = mask_bbox(mask)
            if bbox is None:
                continue
            bx, by, bw, bh = bbox
            bbox_area = bw * bh
            if bbox_area > 0 and area / bbox_area < SUBIMAGE_MIN_BBOX_FILL:
                continue

            usable.append((mask, bbox, region["confidence"]))

        # Filter out container regions: image-wide masks that envelop individual
        # panels. Unlike entity detection (which uses dedup + exclusive pixel
        # claiming), composite detection needs to identify and discard the
        # container to preserve the constituent sub-images.
        if len(usable) > 1:
            containers: set[int] = set()
            for i in range(len(usable)):
                for j in range(i + 1, len(usable)):
                    containment = compute_containment(usable[i][0], usable[j][0])
                    if containment > SUBIMAGE_CONTAINMENT_THRESHOLD:
                        # Remove the larger region (the container)
                        area_i = int(usable[i][0].sum())
                        area_j = int(usable[j][0].sum())
                        if area_i >= area_j:
                            containers.add(i)
                        else:
                            containers.add(j)
            if containers:
                usable = [r for idx, r in enumerate(usable) if idx not in containers]

        # Fallback if 0 or 1 usable regions
        if len(usable) <= 1:
            return [full_bounds]

        # Check pairwise overlap — if any pair has IoU > threshold, fallback
        for i in range(len(usable)):
            for j in range(i + 1, len(usable)):
                if compute_iou(usable[i][0], usable[j][0]) > SUBIMAGE_MAX_OVERLAP_IOU:
                    return [full_bounds]

        # Convert to SubimageBounds
        result = []
        for mask, (bx, by, bw, bh), _ in usable:
            # Crop mask to bbox coordinate space and encode as RLE
            crop_mask = mask[by : by + bh, bx : bx + bw]
            crop_counts = _encode_rle_mask(crop_mask)
            result.append(
                {
                    "bbox": {"x": bx, "y": by, "width": bw, "height": bh},
                    "mask": {"counts": compress_rle(crop_counts)},
                }
            )

        return result

    def _encode_image(self, image: Image.Image, *, max_dim: int | None = VLM_MAX_IMAGE_DIM) -> str:
        """Encode PIL image to base64 JPEG, optionally resizing first.

        Args:
            image: PIL image to encode.
            max_dim: Maximum dimension (width or height). Pass None to skip resizing.
        """
        if max_dim is not None and max(image.size) > max_dim:
            ratio = max_dim / max(image.size)
            new_size = (int(image.size[0] * ratio), int(image.size[1] * ratio))
            image = image.resize(new_size, Image.Resampling.LANCZOS)

        buffer = io.BytesIO()
        image.save(buffer, format="JPEG", quality=90)
        return base64.b64encode(buffer.getvalue()).decode("utf-8")

    def _call_vlm(
        self,
        prompt: str,
        original_b64: str,
        annotated_b64: str,
        schema_json: str,
        num_regions: int,
    ) -> dict[str, Any]:
        """Call the VLM model with multi-image input.

        Uses Qwen3-VL ChatML format with separate image inputs.
        Uses structured_outputs for guaranteed JSON conformance.
        """
        region_hint = f"""
The annotated image contains {num_regions} labeled regions (0 through {num_regions - 1}).
Each region has a circled number label. Provide an entry for EVERY numbered region
in the regions array. For regions that aren't built structures (trees, sky, streets,
vehicles), use "non_structure" as the entity_type with an empty description.

Each region has a "surroundings" field. Use the schema to see valid values for
non_entity types and spatial relationship types. Relationships can be listed from
both sides for validation.
"""
        text_prompt = (
            "<|im_start|>user\n"
            f"{prompt}\n"
            "<|vision_start|><|image_pad|><|vision_end|>"  # Original image
            "<|vision_start|><|image_pad|><|vision_end|>"  # Annotated image
            f"{region_hint}"
            "<|im_end|>\n"
            "<|im_start|>assistant\n"
        )

        schema = json.loads(schema_json)
        sampling_params = json.dumps(
            {
                "max_tokens": 32768,
                "temperature": 0.1,
                "structured_outputs": json.dumps({"json": schema}),
            }
        )

        image_array = np.array([original_b64.encode("utf-8"), annotated_b64.encode("utf-8")])

        vlm_request = pb_utils.InferenceRequest(
            model_name="vlm",
            requested_output_names=["text_output"],
            inputs=[
                pb_utils.Tensor("text_input", np.array([text_prompt.encode("utf-8")])),
                pb_utils.Tensor("image", image_array),
                pb_utils.Tensor("sampling_parameters", np.array([sampling_params.encode("utf-8")])),
                pb_utils.Tensor("exclude_input_in_output", np.array([True])),
            ],
        )
        vlm_request.set_timeout_ms(VLM_TIMEOUT_MS)
        vlm_responses = vlm_request.exec(decoupled=True)

        first = next(vlm_responses, None)
        if first is None:
            return {"error": "VLM returned no response"}
        second = next(vlm_responses, None)
        if second is not None:
            return {"error": "VLM returned multiple responses (streaming not supported)"}

        vlm_response = first
        if vlm_response.has_error():
            return {"error": f"VLM error: {vlm_response.error().message()}"}

        output_tensor = pb_utils.get_output_tensor_by_name(vlm_response, "text_output")
        output_text = get_string_from_tensor(output_tensor)

        # Parse thinking content from Qwen3-VL-Thinking output.
        thinking_content = None
        json_text = output_text

        if "</think>" in output_text:
            parts = output_text.split("</think>", 1)
            thinking_content = parts[0].replace("<think>", "").strip()
            json_text = parts[1].strip() if len(parts) > 1 else ""

        result: dict[str, Any] = json.loads(json_text)
        if thinking_content:
            result["thinking"] = thinking_content
        return result

    def _assemble_analyzed(
        self,
        vlm: dict[str, Any],
        compressed_regions: list[dict],
        si_idx: int,
        embeddings_map: dict[tuple[int, str, int | None], list[float] | None],
        subimage_embedding: list[float] | None,
    ) -> dict[str, Any]:
        """Assemble an analyzed SubimageAnalysis from VLM + SAM3 + DINOv3 results.

        Note: The conditional inclusion of embedding/thinking keys mirrors the
        Rust-side ``#[serde(skip_serializing_if = "Option::is_none")]``. The
        cross-language ``print_schema validate`` test catches drift.
        """
        vlm_regions = vlm["regions"]  # Now a list, not a dict

        if len(vlm_regions) != len(compressed_regions):
            raise RuntimeError(
                f"VLM returned {len(vlm_regions)} regions but SAM3 detected "
                f"{len(compressed_regions)}: region count must match"
            )

        # Merge SAM3 regions with VLM analysis and DINOv3 embeddings
        unified_regions = []
        for ri, sam_region in enumerate(compressed_regions):
            vlm_analysis = vlm_regions[ri]

            # DINOv3 embedding (None if not computed)
            region_embedding = embeddings_map.get((si_idx, "region", ri))

            region_dict: dict[str, Any] = {
                "segmentation_confidence": sam_region["confidence"],
                "mask": sam_region["mask"],
                "analysis": vlm_analysis,
            }
            if region_embedding is not None:
                region_dict["embedding"] = region_embedding
            unified_regions.append(region_dict)

        # VLM output has scene-level fields nested under "scene"
        vlm_scene = vlm["scene"]

        result: dict[str, Any] = {
            "status": "analyzed",
            "scene": vlm_scene,
            "regions": unified_regions,
        }
        if vlm.get("thinking") is not None:
            result["thinking"] = vlm["thinking"]
        if subimage_embedding is not None:
            result["embedding"] = subimage_embedding
        return result

    def _crop_region_bbox(
        self, image: Image.Image, rle_mask: dict[str, str], padding: float = 0.05
    ) -> Image.Image:
        """Crop image to bounding box of an RLE mask with padding."""
        counts = decompress_rle(rle_mask["counts"])
        mask = decode_rle(counts, image.height, image.width)

        bbox = mask_bbox(mask)
        if bbox is None:
            return image

        x_min, y_min, w, h = bbox
        x_max = x_min + w - 1
        y_max = y_min + h - 1

        pad_x = int(w * padding)
        pad_y = int(h * padding)

        x_min = max(0, x_min - pad_x)
        y_min = max(0, y_min - pad_y)
        x_max = min(image.width, x_max + pad_x)
        y_max = min(image.height, y_max + pad_y)

        return image.crop((x_min, y_min, x_max, y_max))

    def _call_dinov3(self, images: list[Image.Image]) -> list[list[float] | None]:
        """Call DINOv3 model with a list of PIL images.

        Returns list of 1024-dim L2-normalized embeddings.
        """
        b64_images = []
        for img in images:
            buffer = io.BytesIO()
            img.save(buffer, format="JPEG", quality=90)
            b64_images.append(base64.b64encode(buffer.getvalue()).decode("utf-8"))

        image_array = np.array([b.encode("utf-8") for b in b64_images]).reshape(1, -1)

        dino_request = pb_utils.InferenceRequest(
            model_name="dinov3",
            requested_output_names=["embeddings"],
            inputs=[pb_utils.Tensor("images", image_array)],
        )
        dino_request.set_timeout_ms(DINOV3_TIMEOUT_MS)
        dino_response = dino_request.exec()

        if dino_response.has_error():
            pb_utils.Logger.log_error(f"DINOv3 error: {dino_response.error().message()}")
            return [None for _ in range(len(images))]

        embeddings_tensor = pb_utils.get_output_tensor_by_name(dino_response, "embeddings")
        embeddings_json = get_string_from_tensor(embeddings_tensor)
        result: list[list[float] | None] = json.loads(embeddings_json)
        return result

    def finalize(self):
        """Clean up."""
        pass


# ==================== Module-level helpers ====================


# NOTE: Duplicated as encode_rle() in sam3/1/model.py. Triton's Python
# backend loads each model in isolation, so there's no clean way to share
# code between models without complicating the deployment structure.
def _encode_rle_mask(mask: np.ndarray) -> list[int]:
    """Encode a binary mask as RLE (COCO-style, column-major order)."""
    flat = mask.flatten(order="F")
    counts: list[int] = []
    current_val = 0
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


def _full_image_bounds(width: int, height: int) -> dict[str, Any]:
    """Create SubimageBounds covering the full image."""
    return {
        "bbox": {"x": 0, "y": 0, "width": width, "height": height},
        "mask": {"counts": make_full_image_mask(height, width)},
    }


def _make_response(result: dict[str, Any]) -> Any:
    """Wrap a result dict into a Triton InferenceResponse."""
    result_json = json.dumps(result)
    output_tensor = pb_utils.Tensor("result", np.array([result_json.encode("utf-8")]))
    return pb_utils.InferenceResponse([output_tensor])
