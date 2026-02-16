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
from dataclasses import dataclass
from typing import Any, TypedDict

import numpy as np
import triton_python_backend_utils as pb_utils
from PIL import Image, ImageDraw, ImageFont

# Import from same directory (needed for both Triton and test environments)
sys.path.insert(0, os.path.dirname(__file__))
from baml_converter import jsonschema_to_baml

# Max regions per subimage (caps SAM3 post-processing and DINOv3 embedding count).
MAX_REGIONS = 32

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
MAX_IMAGE_BYTES = 10 * 1024 * 1024  # 10MB limit to prevent OOM

# Max image dimension (longest edge) before sending to VLM.
# Qwen3-VL: 28x28 pixels = 1 token, max 16384 tokens/image.
# 2048x2048 = ~5.4K tokens × 2 images = ~11K tokens, well within 128K context.
VLM_MAX_IMAGE_DIM = 2048


# Timeouts are generous placeholder values — no production latency data yet.
# VLM with 32K max_tokens on a 72B model can take minutes for complex images.
SAM3_TIMEOUT_US = 120_000_000  # 2 min
VLM_TIMEOUT_US = 300_000_000  # 5 min
DINOV3_TIMEOUT_US = 60_000_000  # 1 min
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

# Text prompts for SAM3 entity segmentation. These are the top-level
# categories we ask SAM3 to detect. Each additional prompt costs ~420ms
# (the image encoder runs once and is shared across all prompts).
ENTITY_PROMPTS = ["building", "bridge", "tower", "monument", "infrastructure"]

# Hierarchical region detection: prompts whose regions should become sub-features
# of a parent entity when spatially contained. Key = child prompt, value = set of
# valid parent prompts. Only one level of nesting.
SUBORDINATE_PROMPTS: dict[str, set[str]] = {
    "tower": {"building", "infrastructure", "monument"},
}
CONTAINMENT_THRESHOLD = 0.7
DEDUP_IOU_THRESHOLD = 0.7
MAX_FEATURES_PER_ENTITY = 8


@dataclass
class _MaskEntry:
    """A decoded mask with metadata, used throughout the postprocessing pipeline.

    Replaces parallel arrays / co-indexed tuples to avoid index-mapping bugs.
    The ``tag`` field is an opaque identity marker that survives pipeline stages
    (dedup, exclusivity, sorting) so callers can track which original entry a
    result corresponds to.
    """

    mask: np.ndarray
    score: float
    prompt: str = ""
    tag: int = -1


class _SubimageData(TypedDict):
    """Intermediate per-subimage data passed between pipeline stages."""

    bounds: dict[str, Any]
    vlm: dict[str, Any] | None
    entities: list[dict[str, Any]]
    crop_b64: str
    crop_height: int
    crop_width: int


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
    entries: list[_MaskEntry], iou_threshold: float = DEDUP_IOU_THRESHOLD
) -> list[_MaskEntry]:
    """Remove duplicate masks based on IoU threshold.

    Higher-scoring entries are kept first; lower-scoring entries that overlap
    above the threshold are discarded.
    """
    if not entries:
        return []

    sorted_entries = sorted(entries, key=lambda e: e.score, reverse=True)

    keep: list[_MaskEntry] = []
    for entry in sorted_entries:
        is_duplicate = any(compute_iou(entry.mask, kept.mask) > iou_threshold for kept in keep)
        if not is_duplicate:
            keep.append(entry)

    return keep


def make_masks_exclusive(
    entries: list[_MaskEntry],
    survival_threshold: float = 0.1,
) -> list[_MaskEntry]:
    """Make masks mutually exclusive using confidence-based pixel claiming.

    Higher confidence entries claim pixels first. Entries that lose too many
    pixels (below survival_threshold of original) are removed.

    Returns a new list of ``_MaskEntry`` with non-overlapping masks.
    Metadata (score, prompt) is preserved on each surviving entry.
    """
    if not entries:
        return []

    h, w = entries[0].mask.shape
    claimed = np.zeros((h, w), dtype=bool)

    result: list[_MaskEntry] = []
    for entry in entries:
        original_pixels = entry.mask.sum()
        if original_pixels == 0:
            continue

        # Claim only unclaimed pixels
        #
        # WORKAROUND: `mask & ~claimed` silently corrupts `claimed` on
        # numpy 1.26.x + Python 3.14. The ~ operator goes through CPython's
        # nb_invert slot, which in 3.14 can decrement the local variable's
        # refcount before calling the ufunc. numpy's temporary elision sees
        # refcount == 1 on arrays >= 2^18 elements and reuses the buffer
        # in-place, so ~claimed mutates claimed and returns the same object.
        # Calling np.logical_not() (or np.invert()) avoids this because the
        # function-call path keeps an extra reference alive via the argument.
        # Fixed in numpy 2.x (numpy/numpy#29685). Remove this workaround
        # once SAM3 drops the numpy <2 pin.
        # See: https://github.com/numpy/numpy/issues/28681
        exclusive_mask = entry.mask & np.logical_not(claimed)
        remaining_pixels = exclusive_mask.sum()

        survival_ratio = remaining_pixels / original_pixels
        if survival_ratio >= survival_threshold:
            claimed |= exclusive_mask.astype(bool)
            result.append(
                _MaskEntry(
                    mask=exclusive_mask.astype(entry.mask.dtype),
                    score=entry.score,
                    prompt=entry.prompt,
                    tag=entry.tag,
                )
            )

    return result


def sort_regions_left_to_right(entries: list[_MaskEntry]) -> list[_MaskEntry]:
    """Sort regions by centroid x-coordinate (left to right).

    This makes region numbering predictable for the VLM - region 1 is leftmost.
    """
    return sorted(entries, key=lambda e: compute_centroid_x(e.mask))


def group_by_containment(
    entries: list[_MaskEntry],
) -> tuple[list[_MaskEntry], dict[int, list[_MaskEntry]]]:
    """Group regions into parent entities and child features using spatial containment.

    A region becomes a child of a larger region when:
    1. Its prompt is in SUBORDINATE_PROMPTS
    2. The larger region's prompt is a valid parent for that subordinate
    3. The smaller region is >CONTAINMENT_THRESHOLD contained in the larger

    On equal containment, the smallest valid parent (tightest fit) wins.

    Returns (top_level, children) where children maps top-level index to child list.
    """
    if not entries:
        return [], {}

    # Track which indices are claimed as children
    child_of: dict[int, int] = {}  # child_idx -> parent_idx

    for i, child in enumerate(entries):
        area_i = int(child.mask.sum())

        # Only subordinate prompts can become children
        if child.prompt not in SUBORDINATE_PROMPTS:
            continue

        valid_parents = SUBORDINATE_PROMPTS[child.prompt]
        best_parent: int | None = None
        best_containment = CONTAINMENT_THRESHOLD
        best_parent_area = float("inf")

        for j, parent in enumerate(entries):
            if i == j:
                continue

            area_j = int(parent.mask.sum())

            # Parent must be larger and have a valid prompt
            if area_j <= area_i or parent.prompt not in valid_parents:
                continue

            # Check containment of smaller (i) within larger (j)
            intersection = float(np.logical_and(child.mask, parent.mask).sum())
            containment = intersection / area_i if area_i > 0 else 0.0

            # Prefer higher containment; on tie, prefer smaller parent (tighter fit)
            if containment > best_containment or (
                containment == best_containment and area_j < best_parent_area
            ):
                best_containment = containment
                best_parent = j
                best_parent_area = area_j

        if best_parent is not None:
            child_of[i] = best_parent

    # Build top-level and children lists
    top_level_indices = [i for i in range(len(entries)) if i not in child_of]
    new_idx = {old: new for new, old in enumerate(top_level_indices)}

    top_level = [entries[i] for i in top_level_indices]
    children: dict[int, list[_MaskEntry]] = {}
    for child_idx, parent_idx in child_of.items():
        if parent_idx in new_idx:
            new_parent = new_idx[parent_idx]
            children.setdefault(new_parent, []).append(entries[child_idx])

    return top_level, children


def postprocess_entity_regions(raw_regions: list[dict], height: int, width: int) -> list[dict]:
    """Post-process raw SAM3 output into hierarchical entity structure.

    Pipeline:
    1. Decode all masks into ``_MaskEntry`` objects
    2. Deduplicate (IoU-based, prompt-agnostic)
    3. Group by containment -> top-level entities + child features
    4. Union child masks into parent, then make top-level exclusive
    5. Make sibling features exclusive (per parent)
    6. Sort left-to-right, limit to MAX_REGIONS, cap features
    7. Re-encode all masks

    Each entity dict has a ``features`` list of child sub-regions.
    Parent masks keep their FULL mask (including sub-feature pixels).
    """
    if not raw_regions:
        return []

    # Step 1: Decode into _MaskEntry objects (tagged for tracking through pipeline)
    entries = [
        _MaskEntry(
            mask=decode_rle(r["mask"], height, width),
            score=r["confidence"],
            prompt=r.get("prompt", ""),
            tag=i,
        )
        for i, r in enumerate(raw_regions)
    ]

    # Step 2: Deduplicate overlapping masks (IoU-based, prompt-agnostic)
    deduped = deduplicate_masks(entries)

    # Step 3: Group by containment
    top_level, children_map = group_by_containment(deduped)

    # Step 3b: Union child masks into parent — extend parent to cover children
    for parent_idx, child_list in children_map.items():
        parent = top_level[parent_idx]
        for child in child_list:
            parent.mask = np.logical_or(parent.mask, child.mask).astype(parent.mask.dtype)

    # Step 4: Make top-level masks exclusive.
    # Tags survive through make_masks_exclusive, so we can match parents.
    top_exclusive = make_masks_exclusive(top_level)

    # Step 5: For each surviving parent, make sibling features exclusive.
    # Stay in tag-space so sorting/filtering doesn't invalidate mappings.
    surviving_tags = {e.tag for e in top_exclusive}
    children_by_tag: dict[int, list[_MaskEntry]] = {}
    for parent_idx, child_list in children_map.items():
        parent_tag = top_level[parent_idx].tag
        if parent_tag in surviving_tags:
            children_by_tag[parent_tag] = make_masks_exclusive(child_list)

    # Step 6: Sort top-level left-to-right, limit to MAX_REGIONS
    sorted_top = sort_regions_left_to_right(top_exclusive)[:MAX_REGIONS]

    # Step 7: Re-encode to hierarchical structure
    result = []
    for entry in sorted_top:
        entity: dict[str, Any] = {
            "confidence": entry.score,
            "mask": _encode_rle_mask(entry.mask.astype(np.uint8)),
            "prompt": entry.prompt,
            "features": [],
        }

        # Add children, sorted left-to-right, capped
        child_list = children_by_tag.get(entry.tag, [])
        if child_list:
            child_sorted = sort_regions_left_to_right(child_list)
            for child in child_sorted[:MAX_FEATURES_PER_ENTITY]:
                entity["features"].append(
                    {
                        "confidence": child.score,
                        "mask": _encode_rle_mask(child.mask.astype(np.uint8)),
                        "prompt": child.prompt,
                    }
                )

        result.append(entity)

    return result


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

For media_type classification:
- "photo_of_model" requires clear evidence of a physical scale model (visible base,
  miniature details, model railroad context). Do NOT use this for unusual camera
  angles, aerial views, or tilt-shift effects on real buildings.

For "analyzed" responses:
- You will see two images showing exactly the same scene:
  1. The original photograph - use this to see fine details clearly
  2. The annotated photograph - same image with colored overlays marking detected
     regions, each labeled with a circled number
- Find region numbers and boundaries in the ANNOTATED image
  (look for numbers like 1, 2, 3 inside colored circles)
- Examine the corresponding area in the ORIGINAL image for details
- The annotations may partially obscure details, so always cross-reference
  with the original"""


class TritonPythonModel:
    """BLS orchestration model for the subimage-centric analysis pipeline."""

    def initialize(self, args):
        """Initialize the orchestrator."""
        self.model_config = json.loads(args["model_config"])

        # Model IDs for provenance tracking.
        # model_repository points to this model's own dir (e.g. /models/analysis),
        # so go up one level to reach the root model repository.
        # VLM and DINOv3 model.json files are authoritative (used to load the model).
        # SAM3 has no model.json — build_sam3_image_model() bakes in the checkpoint
        # with no way to parameterize or introspect it. This will improve if/when we
        # convert SAM3 to ONNX and control the checkpoint path ourselves.
        model_repo_root = os.path.dirname(args.get("model_repository", "/models/analysis"))
        self.vlm_model_name = self._read_model_id(model_repo_root, "vlm")
        self.sam3_model_name = "facebook/sam3"  # see comment above
        self.dinov3_model_name = self._read_model_id(model_repo_root, "dinov3")
        self.git_revision = os.environ.get("ANALYSIS_GIT_SHA", "unknown")

        self.versions = {
            "vlm": self.vlm_model_name,
            "sam3": self.sam3_model_name,
            "dinov3": self.dinov3_model_name,
            "git_sha": self.git_revision,
        }

        pb_utils.Logger.log_info("Analysis BLS model initialized")

    @staticmethod
    def _read_model_id(model_repo_root: str, model_name: str) -> str:
        """Read model ID from a sibling model's model.json."""
        model_json = os.path.join(model_repo_root, model_name, "1", "model.json")
        if os.path.exists(model_json):
            with open(model_json) as f:
                config = json.load(f)
            result: str = config.get("model", "unknown")
            return result
        return "unknown"

    def execute(self, requests):
        """Process analysis requests through the subimage pipeline.

        Pipeline: detect subimages → per-subimage (SAM3 → annotate → VLM)
        → batched DINOv3 → assemble.
        """
        responses = []

        for request in requests:
            image_tensor = pb_utils.get_input_tensor_by_name(request, "image")
            schema_tensor = pb_utils.get_input_tensor_by_name(request, "schema")
            skip_vlm_tensor = pb_utils.get_input_tensor_by_name(request, "skip_vlm")

            image_b64 = get_string_from_tensor(image_tensor)
            schema_json = get_string_from_tensor(schema_tensor)
            skip_vlm = (
                bool(skip_vlm_tensor.as_numpy().flatten()[0])
                if skip_vlm_tensor is not None
                else False
            )

            # Validate image size to prevent OOM
            estimated_size = len(image_b64) * 3 // 4
            if estimated_size > MAX_IMAGE_BYTES:
                size_mb = estimated_size // (1024 * 1024)
                limit_mb = MAX_IMAGE_BYTES // (1024 * 1024)
                responses.append(
                    _make_response(
                        {
                            "outcome": "image_rejected",
                            "reason": f"Image too large: {size_mb}MB exceeds {limit_mb}MB limit",
                        }
                    )
                )
                continue

            # Decode image
            image_bytes = base64.b64decode(image_b64)
            original = Image.open(io.BytesIO(image_bytes)).convert("RGB")

            # Step 1: Detect subimages (panels) in the source image
            subimage_bounds = self._detect_subimages(image_b64, original)

            # Step 2: Per-subimage analysis
            subimage_results: list[_SubimageData] = []

            # Pre-compute prompt once (only used if VLM runs)
            vlm_prompt = build_vlm_prompt(schema_json) if not skip_vlm else None

            # Per-subimage SAM3 segmentation
            vlm_inputs: list[tuple[int, str, str, int]] = []
            prompts_array = np.array([p.encode("utf-8") for p in ENTITY_PROMPTS])
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
                    inputs=[
                        pb_utils.Tensor("image", np.array([[crop_b64.encode("utf-8")]])),
                        pb_utils.Tensor("prompts", prompts_array.reshape(1, -1)),
                    ],
                    timeout=SAM3_TIMEOUT_US,
                )
                sam_response = sam_request.exec()
                if sam_response.has_error():
                    raise RuntimeError(
                        f"SAM3 error on subimage {si_idx}: {sam_response.error().message()}"
                    )

                regions_tensor = pb_utils.get_output_tensor_by_name(sam_response, "regions")
                regions_json = get_string_from_tensor(regions_tensor)
                raw_regions = json.loads(regions_json)

                # Post-process: dedup, containment grouping, exclusive, sort, limit
                entities = postprocess_entity_regions(raw_regions, crop.height, crop.width)

                # Compress masks for wire transfer
                compressed_entities = []
                for ent in entities:
                    c_ent = {
                        **ent,
                        "mask": {"counts": compress_rle(ent["mask"])},
                        "features": [
                            {**f, "mask": {"counts": compress_rle(f["mask"])}}
                            for f in ent.get("features", [])
                        ],
                    }
                    compressed_entities.append(c_ent)

                si_data: _SubimageData = {
                    "bounds": bounds,
                    "vlm": None,
                    "entities": compressed_entities,
                    "crop_b64": crop_b64,
                    "crop_height": crop.height,
                    "crop_width": crop.width,
                }
                subimage_results.append(si_data)

                if not skip_vlm:
                    # Prepare VLM input: annotate with top-level entities only
                    # (sub-features are structural metadata, not VLM targets)
                    annotated = annotate_image(crop, entities)
                    original_b64 = self._encode_image(crop)
                    annotated_b64 = self._encode_image(annotated)
                    vlm_inputs.append((si_idx, original_b64, annotated_b64, len(entities)))

            # Step 2b: Run VLM concurrently, then update subimage results
            if vlm_inputs:
                if vlm_prompt is None:
                    raise RuntimeError("vlm_prompt is None but VLM analysis requested")
                vlm_results = self._run_vlm_concurrent(vlm_inputs, vlm_prompt, schema_json)
                for si_idx, vlm_result in vlm_results.items():
                    subimage_results[si_idx]["vlm"] = vlm_result

            # Step 3: DINOv3 embeddings (one forward pass per subimage)
            # Keys: (si_idx, entity_idx, feature_idx), -1 = N/A
            # e.g. (0, -1, -1) = CLS, (0, 2, -1) = entity 2
            embeddings_map: dict[tuple[int, int, int], list[float] | None] = {}
            for si_idx, si_data in enumerate(subimage_results):
                needs_embedding = (
                    si_data["vlm"] is None  # skip_vlm
                    or si_data["vlm"].get("status") == "analyzed"
                )
                if not needs_embedding:
                    continue

                sub_emb, entity_embs, feature_embs = self._call_dinov3(
                    si_data["crop_b64"],
                    si_data["crop_height"],
                    si_data["crop_width"],
                    si_data["entities"],
                )
                embeddings_map[(si_idx, -1, -1)] = sub_emb
                for ei, emb in enumerate(entity_embs):
                    embeddings_map[(si_idx, ei, -1)] = emb
                for (ei, fi), emb in feature_embs.items():
                    embeddings_map[(si_idx, ei, fi)] = emb

            # Step 4: Assemble final AnalysisResult
            subimages = []
            for si_idx, si_data in enumerate(subimage_results):
                vlm = si_data["vlm"]
                bounds = si_data["bounds"]
                entities = si_data["entities"]

                if vlm is None:
                    # VLM was skipped — return Segmented variant
                    subimage_embedding = embeddings_map.get((si_idx, -1, -1))
                    analysis = self._assemble_segmented(
                        entities, si_idx, embeddings_map, subimage_embedding
                    )
                elif vlm.get("status") == "analyzed":
                    subimage_embedding = embeddings_map.get((si_idx, -1, -1))
                    analysis = self._assemble_analyzed(
                        vlm, entities, si_idx, embeddings_map, subimage_embedding
                    )
                elif vlm.get("status") == "rejected":
                    analysis = {"status": "rejected", "reason": vlm.get("reason", "Unknown")}
                elif "error" in vlm:
                    analysis = {"status": "error", "message": vlm["error"]}
                else:
                    analysis = {"status": "error", "message": "Unexpected VLM output format"}

                subimages.append({"bounds": bounds, "analysis": analysis})

            result: dict[str, Any] = {
                "outcome": "success",
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
            timeout=SAM3_TIMEOUT_US,
        )
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

        # Sort in row-major order
        usable.sort(key=lambda r: (r[1][1], r[1][0]))

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

    def _run_vlm_concurrent(
        self,
        vlm_inputs: list[tuple[int, str, str, int]],
        vlm_prompt: str,
        schema_json: str,
    ) -> dict[int, dict[str, Any]]:
        """Run VLM analysis concurrently across subimages.

        Args:
            vlm_inputs: List of (si_idx, original_b64, annotated_b64, n_regions).
            vlm_prompt: Pre-built VLM prompt string.
            schema_json: JSON schema for structured output.

        Returns:
            Map from subimage index to VLM result dict.
        """
        results: dict[int, dict[str, Any]] = {}
        with ThreadPoolExecutor(max_workers=min(len(vlm_inputs), 8)) as pool:
            future_to_idx = {
                pool.submit(
                    self._call_vlm,
                    vlm_prompt,
                    orig_b64,
                    ann_b64,
                    schema_json,
                    n_regions,
                ): si_idx
                for si_idx, orig_b64, ann_b64, n_regions in vlm_inputs
            }
            for future in as_completed(future_to_idx, timeout=VLM_TIMEOUT_US / 1_000_000 + 30):
                idx = future_to_idx[future]
                try:
                    results[idx] = future.result(timeout=FUTURE_RESULT_TIMEOUT_S)
                except (json.JSONDecodeError, KeyError, TimeoutError) as e:
                    results[idx] = {"error": f"VLM call failed: {e}"}
        return results

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

Each region has a "surroundings" field with spatial relationships. IMPORTANT: each
region's related_regions must ONLY reference regions with a LOWER index than itself.
Region 0 has no related_regions. Region 1 can reference region 0. Region 2 can
reference regions 0 and 1. And so on. All spatial relations are symmetric, so the
system reconstructs the full graph from these lower-index-only references.
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
            timeout=VLM_TIMEOUT_US,
        )
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

        # Strip thinking tags if present (Qwen3-VL-Thinking wraps output in <think>...</think>).
        json_text = output_text
        if "</think>" in output_text:
            json_text = output_text.split("</think>", 1)[1].strip()

        result: dict[str, Any] = json.loads(json_text)
        return result

    def _build_region_dicts(
        self,
        entities: list[dict],
        si_idx: int,
        embeddings_map: dict[tuple[int, int, int], list[float] | None],
        vlm_regions: list[dict[str, Any]] | None = None,
    ) -> list[dict[str, Any]]:
        """Build unified region dicts from hierarchical SAM3 + optional VLM + DINOv3.

        Each entity becomes a region dict with a "features" list of sub-regions.
        VLM annotations apply only to top-level entities.
        """
        if vlm_regions is not None and len(vlm_regions) != len(entities):
            raise RuntimeError(
                f"VLM returned {len(vlm_regions)} regions but SAM3 detected "
                f"{len(entities)}: region count must match"
            )

        regions = []
        for ei, entity in enumerate(entities):
            region_dict: dict[str, Any] = {
                "segmentation_confidence": entity["confidence"],
                "detected_as": entity.get("prompt", ""),
                "mask": entity["mask"],
            }
            if vlm_regions is not None:
                region_dict["analysis"] = vlm_regions[ei]
            entity_embedding = embeddings_map.get((si_idx, ei, -1))
            if entity_embedding is not None:
                region_dict["embedding"] = entity_embedding

            # Build feature sub-regions
            features = []
            for fi, feature in enumerate(entity.get("features", [])):
                feat_dict: dict[str, Any] = {
                    "segmentation_confidence": feature["confidence"],
                    "detected_as": feature.get("prompt", ""),
                    "mask": feature["mask"],
                    "features": [],
                }
                feat_embedding = embeddings_map.get((si_idx, ei, fi))
                if feat_embedding is not None:
                    feat_dict["embedding"] = feat_embedding
                features.append(feat_dict)
            region_dict["features"] = features

            regions.append(region_dict)
        return regions

    def _assemble_analyzed(
        self,
        vlm: dict[str, Any],
        entities: list[dict],
        si_idx: int,
        embeddings_map: dict[tuple[int, int, int], list[float] | None],
        subimage_embedding: list[float] | None,
    ) -> dict[str, Any]:
        """Assemble an analyzed SubimageAnalysis from VLM + SAM3 + DINOv3 results."""
        regions = self._build_region_dicts(
            entities, si_idx, embeddings_map, vlm_regions=vlm["regions"]
        )
        result: dict[str, Any] = {
            "status": "analyzed",
            "scene": vlm["scene"],
            "regions": regions,
        }
        if subimage_embedding is not None:
            result["embedding"] = subimage_embedding
        return result

    def _assemble_segmented(
        self,
        entities: list[dict],
        si_idx: int,
        embeddings_map: dict[tuple[int, int, int], list[float] | None],
        subimage_embedding: list[float] | None,
    ) -> dict[str, Any]:
        """Assemble a Segmented SubimageAnalysis (VLM skipped, SAM3 + DINOv3 only)."""
        regions = self._build_region_dicts(entities, si_idx, embeddings_map)
        result: dict[str, Any] = {
            "status": "segmented",
            "regions": regions,
        }
        if subimage_embedding is not None:
            result["embedding"] = subimage_embedding
        return result

    def _call_dinov3(
        self,
        image_b64: str,
        crop_height: int,
        crop_width: int,
        entities: list[dict[str, Any]],
    ) -> tuple[
        list[float] | None,
        list[list[float] | None],
        dict[tuple[int, int], list[float] | None],
    ]:
        """Call DINOv3 for one subimage: CLS + entity + feature embeddings.

        Sends all masks (entities + their features) in one call. Maps
        embeddings back to (entity_idx, feature_idx) pairs.

        Returns (subimage_embedding, entity_embeddings, feature_embeddings_map).
        """
        mask_specs: list[dict[str, Any] | None] = [None]  # index 0: CLS
        index_map: list[tuple[int, int | None]] = []  # (entity_idx, feature_idx_or_None)

        for ei, entity in enumerate(entities[:MAX_REGIONS]):
            mask_specs.append(
                {
                    "counts": entity["mask"]["counts"],
                    "height": crop_height,
                    "width": crop_width,
                }
            )
            index_map.append((ei, None))
            for fi, feature in enumerate(entity.get("features", [])[:MAX_FEATURES_PER_ENTITY]):
                mask_specs.append(
                    {
                        "counts": feature["mask"]["counts"],
                        "height": crop_height,
                        "width": crop_width,
                    }
                )
                index_map.append((ei, fi))

        masks_json = json.dumps(mask_specs)

        dino_request = pb_utils.InferenceRequest(
            model_name="dinov3",
            requested_output_names=["embeddings"],
            inputs=[
                pb_utils.Tensor("image", np.array([[image_b64.encode("utf-8")]])),
                pb_utils.Tensor("masks", np.array([[masks_json.encode("utf-8")]])),
            ],
            timeout=DINOV3_TIMEOUT_US,
        )
        dino_response = dino_request.exec()

        n_entities = min(len(entities), MAX_REGIONS)
        if dino_response.has_error():
            pb_utils.Logger.log_error(f"DINOv3 error: {dino_response.error().message()}")
            feature_map: dict[tuple[int, int], list[float] | None] = {}
            for ei, ent in enumerate(entities[:MAX_REGIONS]):
                for fi in range(len(ent.get("features", []))):
                    feature_map[(ei, fi)] = None
            return None, [None] * n_entities, feature_map

        embeddings_tensor = pb_utils.get_output_tensor_by_name(dino_response, "embeddings")
        embeddings_json = get_string_from_tensor(embeddings_tensor)
        all_embeddings: list[list[float]] = json.loads(embeddings_json)

        # Map embeddings back: index 0 is CLS, rest follow index_map
        subimage_emb = all_embeddings[0]
        entity_embs: list[list[float] | None] = [None] * n_entities
        feature_embs: dict[tuple[int, int], list[float] | None] = {}

        for map_idx, (eidx, fidx) in enumerate(index_map):
            emb = all_embeddings[map_idx + 1]  # +1 because index 0 is CLS
            if fidx is None:
                entity_embs[eidx] = emb
            else:
                feature_embs[(eidx, fidx)] = emb

        return subimage_emb, entity_embs, feature_embs

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
