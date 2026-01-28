"""BLS orchestration model for the full analysis pipeline.

Orchestrates: SAM3 segmentation → image annotation → VLM analysis.
"""

import base64
import io
import json
import os
import sys
from typing import Any

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

    for region in regions:
        region_id = region["region_id"]
        color = REGION_COLORS[(region_id - 1) % len(REGION_COLORS)]

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
            label = str(region_id)
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
    """Build the VLM prompt for analysis.

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

You will see two images showing exactly the same scene:
1. The original photograph - use this to see fine details clearly
2. The annotated photograph - same image with colored overlays marking detected
   regions, each labeled with a circled number

HOW TO USE THE TWO IMAGES:
- Find region numbers and boundaries in the ANNOTATED image
  (look for numbers like 1, 2, 3 inside colored circles)
- Examine the corresponding area in the ORIGINAL image for architectural
  details, text, and features
- The annotations may partially obscure details, so always cross-reference
  with the original"""


class TritonPythonModel:
    """BLS orchestration model for the full analysis pipeline."""

    def initialize(self, args):
        """Initialize the orchestrator."""
        self.model_config = json.loads(args["model_config"])
        pb_utils.Logger.log_info("Analysis BLS model initialized")

    def execute(self, requests):
        """Process analysis requests by orchestrating SAM3 and VLM."""
        responses = []

        for request in requests:
            image_tensor = pb_utils.get_input_tensor_by_name(request, "image")
            schema_tensor = pb_utils.get_input_tensor_by_name(request, "schema")

            image_b64 = get_string_from_tensor(image_tensor)
            schema_json = get_string_from_tensor(schema_tensor)

            # Validate image size to prevent OOM
            estimated_size = len(image_b64) * 3 // 4  # base64 decode estimate
            if estimated_size > MAX_IMAGE_BYTES:
                size_mb = estimated_size // (1024 * 1024)
                limit_mb = MAX_IMAGE_BYTES // (1024 * 1024)
                # Can't safely decode oversized images, use (0, 0) as placeholder
                result = self._error_result(
                    f"Image too large: {size_mb}MB exceeds {limit_mb}MB limit", 0, 0
                )
                result_json = json.dumps(result)
                output_tensor = pb_utils.Tensor("result", np.array([result_json.encode("utf-8")]))
                responses.append(pb_utils.InferenceResponse([output_tensor]))
                continue

            # Decode image early to get dimensions for all code paths
            image_bytes = base64.b64decode(image_b64)
            original = Image.open(io.BytesIO(image_bytes)).convert("RGB")

            # 1. Call SAM3 for segmentation
            sam_request = pb_utils.InferenceRequest(
                model_name="sam3",
                requested_output_names=["regions"],
                inputs=[pb_utils.Tensor("image", np.array([[image_b64.encode("utf-8")]]))],
            )
            sam_response = sam_request.exec()

            if sam_response.has_error():
                error_msg = sam_response.error().message()
                result = self._error_result(
                    f"SAM3 error: {error_msg}", original.height, original.width
                )
            else:
                regions_tensor = pb_utils.get_output_tensor_by_name(sam_response, "regions")
                regions_json = get_string_from_tensor(regions_tensor)
                regions = json.loads(regions_json)

                # 2. Annotate image
                annotated = annotate_image(original, regions)

                # Encode images for VLM
                original_b64 = self._encode_image(original)
                annotated_b64 = self._encode_image(annotated)

                # 3. Build VLM prompt with schema for context
                prompt = build_vlm_prompt(schema_json)

                # 4. Call VLM with both images and structured output constraint
                vlm_result = self._call_vlm(
                    prompt, original_b64, annotated_b64, schema_json, len(regions)
                )

                # 5. Combine results (compress masks for wire transfer)
                compressed_regions = [
                    {**r, "mask": {"counts": compress_rle(r["mask"])}} for r in regions
                ]
                result = {
                    "image_size": [original.height, original.width],
                    "segmentation": compressed_regions,
                    "annotated_image": annotated_b64,
                    "vlm": vlm_result,
                }

            result_json = json.dumps(result)
            output_tensor = pb_utils.Tensor("result", np.array([result_json.encode("utf-8")]))
            responses.append(pb_utils.InferenceResponse([output_tensor]))

        return responses

    def _encode_image(self, image: Image.Image) -> str:
        """Resize and encode PIL image to base64 JPEG."""
        # Resize if needed to stay within VLM token limits
        if max(image.size) > VLM_MAX_IMAGE_DIM:
            ratio = VLM_MAX_IMAGE_DIM / max(image.size)
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
        # Build ChatML prompt with constant prefix for KV cache efficiency.
        # Text instructions come first (cacheable), then variable images.
        #
        # TODO: The original image comes before the annotated image intentionally.
        # This means the VLM's dependency on SAM3 is only for the second image.
        # In principle, we could start VLM prefill on (text + original) while SAM3
        # is still running, then append the annotated image when ready. This would
        # reduce end-to-end latency by overlapping SAM3 and partial VLM processing.
        #
        # The region count hint is placed right before the annotated image to
        # minimize the prefix that depends on SAM3 output.
        region_hint = f"""
The annotated image contains {num_regions} labeled regions (1 through {num_regions}).
Each region has a circled number label. Provide an entry for each numbered region
that contains a built structure. Omit regions that aren't built structures (trees,
sky, streets, vehicles).

RELATIONSHIPS (all symmetric - always use lower region number as subject):
- "adjacent": structures next to each other on the SAME side of a street
- "across_from": structures facing each other on OPPOSITE sides of a street
  or open space
- same_as is rare: only use when segmentation incorrectly split one structure
  into multiple regions
"""
        text_prompt = (
            "<|im_start|>user\n"
            f"{prompt}\n"
            "<|vision_start|><|image_pad|><|vision_end|>"  # Original image
            "<|vision_start|><|image_pad|><|vision_end|>"  # Annotated image (from SAM3)
            f"{region_hint}"
            "<|im_end|>\n"
            "<|im_start|>assistant\n"
        )

        # Build sampling parameters with structured_outputs
        # Note: structured_outputs requires double-encoding:
        # - Inner json.dumps for the {"json": schema} wrapper
        # - Outer json.dumps for the full sampling_params dict
        # See https://github.com/triton-inference-server/server/issues/7897 for more info
        schema = json.loads(schema_json)
        sampling_params = json.dumps(
            {
                "max_tokens": 32768,
                "temperature": 0.1,
                # TODO: thinking_token_budget isn't supported by Triton vLLM backend yet
                "structured_outputs": json.dumps({"json": schema}),
            }
        )

        # Pass images as separate tensor elements (not JSON array).
        # vLLM backend iterates over elements: for img in images.as_numpy()
        image_array = np.array(
            [
                original_b64.encode("utf-8"),
                annotated_b64.encode("utf-8"),
            ]
        )

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
        # vLLM backend uses decoupled mode for streaming support
        vlm_responses = vlm_request.exec(decoupled=True)

        # We expect exactly one response (streaming disabled). Fail loudly if vLLM
        # starts streaming so we notice and handle it properly.
        responses_list = list(vlm_responses)
        if len(responses_list) == 0:
            return {"error": "VLM returned no response"}
        if len(responses_list) > 1:
            n = len(responses_list)
            return {"error": f"VLM returned {n} responses, expected 1 (streaming not supported)"}

        vlm_response = responses_list[0]
        if vlm_response.has_error():
            return {"error": f"VLM error: {vlm_response.error().message()}"}

        output_tensor = pb_utils.get_output_tensor_by_name(vlm_response, "text_output")
        output_text = get_string_from_tensor(output_tensor)

        # Parse thinking content from Qwen3-VL-Thinking output.
        # Format: <think>...reasoning...</think>{"json": ...}
        # Note: The opening <think> may be implicit (added by chat template).
        thinking_content = None
        json_text = output_text

        if "</think>" in output_text:
            parts = output_text.split("</think>", 1)
            thinking_content = parts[0].replace("<think>", "").strip()
            json_text = parts[1].strip() if len(parts) > 1 else ""

        try:
            result: dict[str, Any] = json.loads(json_text)
            if thinking_content:
                result["thinking"] = thinking_content
            return result
        except json.JSONDecodeError as e:
            # If structured_outputs worked, this shouldn't happen
            pb_utils.Logger.log_error(f"JSON parse error: {e}")
            return {"error": f"JSON parse error: {e}", "raw_output": output_text}

    def _error_result(self, error_msg: str, height: int, width: int) -> dict[str, Any]:
        """Create an error result."""
        return {
            "image_size": [height, width],
            "segmentation": [],
            "annotated_image": None,
            "vlm": {"error": error_msg},
        }

    def finalize(self):
        """Clean up."""
        pass
