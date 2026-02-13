#!/usr/bin/env python3
"""Triton model tests.

Run with: pytest test_models.py -v
"""

import base64
import copy
import importlib.util
import io
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

import numpy as np
import numpy.typing as npt
import pytest
from hypothesis import given, settings
from hypothesis import strategies as st
from hypothesis.extra import numpy as hn
from PIL import Image

import mock_triton

# =============================================================================
# Module loading
# =============================================================================

_models_dir = Path(__file__).parent / "models"


def _load_model_module(model_dir: Path, module_name: str, filename: str = "model.py"):
    """Load a Python module from a specific directory as a unique module."""
    model_path = model_dir / filename
    spec = importlib.util.spec_from_file_location(module_name, model_path)
    if spec is None or spec.loader is None:
        raise ImportError(f"Could not load module from {model_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


sam3_module = _load_model_module(_models_dir / "sam3" / "1", "sam3_model")
analysis_model = _load_model_module(_models_dir / "analysis" / "1", "analysis_model")
dinov3_module = _load_model_module(_models_dir / "dinov3" / "1", "dinov3_model")
baml_converter = _load_model_module(
    _models_dir / "analysis" / "1", "baml_converter_test", filename="baml_converter.py"
)

# =============================================================================
# Test helpers
# =============================================================================


def make_test_image(width: int = 200, height: int = 150) -> str:
    """Create a test image and return as base64."""
    img = Image.new("RGB", (width, height), color=(100, 120, 140))
    buffer = io.BytesIO()
    img.save(buffer, format="JPEG", quality=85)
    return base64.b64encode(buffer.getvalue()).decode("utf-8")


def make_test_schema() -> str:
    """Get the actual vlm_schema::SubimageOutput schema from Rust."""
    result = subprocess.run(
        ["cargo", "run", "--bin", "schematool"],
        capture_output=True,
        text=True,
        cwd=Path(__file__).parent.parent,  # analysis/ directory
    )
    if result.returncode != 0:
        pytest.skip("cargo required for schema generation (run from repo with Rust toolchain)")
    return result.stdout


# =============================================================================
# RLE encoding tests
# =============================================================================


class TestRleEncoding:
    """Tests for RLE (run-length encoding) of masks."""

    def test_roundtrip_property_based(self):
        """RLE encode/decode preserves any binary mask (property-based test).

        This catches edge cases like checkerboard patterns, diagonal stripes,
        and other complex shapes that hand-written tests might miss.
        """

        @given(
            mask=hn.arrays(
                dtype=np.uint8,
                shape=st.tuples(
                    st.integers(min_value=1, max_value=100),
                    st.integers(min_value=1, max_value=100),
                ),
                elements=st.sampled_from([0, 1]),
            )
        )
        @settings(max_examples=50, deadline=None)
        def check_roundtrip(*, mask: npt.NDArray[np.uint8]) -> None:
            counts = sam3_module.encode_rle(mask)
            h, w = mask.shape
            decoded = analysis_model.decode_rle(counts, h, w)
            assert np.array_equal(decoded, mask)

        check_roundtrip()

    def test_compression_roundtrip_various_run_lengths(self):
        """RLE compression preserves various run length patterns."""
        test_cases = [
            [0],  # Just zero
            [0, 1],  # Minimal
            [100, 50, 100],  # Typical small
            [10000, 5000, 10000],  # Larger values
            [0, 0, 0, 1, 0],  # Multiple zeros
            [1, 1, 1, 1, 1],  # All ones
            list(range(0, 100)),  # Sequential values
        ]

        for counts in test_cases:
            compressed = analysis_model.compress_rle(counts)
            decoded_counts = analysis_model.decompress_rle(compressed)
            assert decoded_counts == counts, f"Failed for {counts}: got {decoded_counts}"


# =============================================================================
# Mask deduplication tests
# =============================================================================


class TestMaskDeduplication:
    """Tests for mask deduplication logic.

    Deduplication removes masks that overlap significantly (IoU > threshold)
    with higher-confidence masks, keeping only the highest-confidence version.
    """

    def test_removes_duplicate_masks(self):
        """Deduplication removes masks with high IoU overlap."""
        mask1 = np.zeros((10, 10), dtype=np.uint8)
        mask1[0:6, 0:6] = 1

        mask2 = mask1.copy()  # exact duplicate

        mask3 = np.zeros((10, 10), dtype=np.uint8)
        mask3[7:10, 7:10] = 1  # non-overlapping

        result = analysis_model.deduplicate_masks([mask1, mask2, mask3], [0.9, 0.8, 0.7])

        assert len(result) == 2, "Duplicate should be removed"

    def test_empty_input_returns_empty(self):
        """Empty input produces empty output."""
        result = analysis_model.deduplicate_masks([], [])
        assert result == []

    def test_single_mask_passes_through(self):
        """Single mask is returned unchanged."""
        mask = np.zeros((10, 10), dtype=np.uint8)
        mask[2:8, 2:8] = 1

        result = analysis_model.deduplicate_masks([mask], [0.9])

        assert len(result) == 1
        assert np.array_equal(result[0][0], mask)
        assert result[0][1] == 0.9

    def test_keeps_higher_confidence_on_overlap(self):
        """When masks overlap significantly, keeps only the higher-confidence one."""
        mask_high = np.zeros((10, 10), dtype=np.uint8)
        mask_high[0:8, 0:8] = 1  # 64 pixels

        # Nearly identical mask - IoU will be very high
        mask_low = np.zeros((10, 10), dtype=np.uint8)
        mask_low[0:8, 0:8] = 1
        mask_low[0, 0] = 0  # 63 pixels, IoU = 63/64 ≈ 0.98 > 0.7

        # Lower confidence mask listed first, but higher confidence should be kept
        result = analysis_model.deduplicate_masks([mask_low, mask_high], [0.7, 0.9])

        assert len(result) == 1
        assert result[0][1] == 0.9, "Should keep higher confidence mask"

    def test_threshold_boundary_keeps_both(self):
        """Masks with IoU exactly at threshold boundary are both kept."""
        # Two masks with ~70% IoU (at the default 0.7 threshold boundary)
        mask1 = np.zeros((10, 10), dtype=np.uint8)
        mask1[0:7, 0:10] = 1  # 70 pixels

        mask2 = np.zeros((10, 10), dtype=np.uint8)
        mask2[3:10, 0:10] = 1  # 70 pixels, overlap = 40 pixels
        # IoU = 40 / (70 + 70 - 40) = 40/100 = 0.4, well below 0.7

        result = analysis_model.deduplicate_masks([mask1, mask2], [0.9, 0.8])
        assert len(result) == 2, "Non-duplicate masks should both be kept"


# =============================================================================
# Exclusive mask tests
# =============================================================================


class TestExclusiveMasks:
    """Tests for making masks mutually exclusive (no pixel overlap).

    When multiple detected regions overlap, higher-confidence regions claim
    the overlapping pixels. This prevents double-counting in entity analysis.
    """

    def test_higher_confidence_claims_overlapping_pixels(self):
        """Higher confidence mask keeps all pixels; lower loses overlap."""
        mask_high = np.zeros((10, 10), dtype=np.uint8)
        mask_high[2:8, 2:8] = 1  # 36 pixels in center

        mask_low = np.zeros((10, 10), dtype=np.uint8)
        mask_low[4:10, 4:10] = 1  # 36 pixels, overlaps with 16 pixels

        result = analysis_model.make_masks_exclusive([(mask_high, 0.9), (mask_low, 0.7)])

        assert len(result) == 2, "Both masks should survive"
        high_result, low_result = result[0][0], result[1][0]

        assert high_result.sum() == 36, "High confidence mask unchanged"
        assert low_result.sum() == 20, (
            f"Low confidence should have 20 pixels, got {low_result.sum()}"
        )

        overlap = np.logical_and(high_result, low_result).sum()
        assert overlap == 0, "Masks should not overlap"

    def test_prunes_masks_losing_most_pixels(self):
        """Masks losing >90% of pixels are removed to avoid tiny fragments.

        This prevents the VLM from receiving noise fragments that would
        confuse region analysis.
        """
        mask_big = np.zeros((10, 10), dtype=np.uint8)
        mask_big[0:10, 0:10] = 1  # 100 pixels

        mask_small = np.zeros((10, 10), dtype=np.uint8)
        mask_small[4:6, 4:6] = 1  # 4 pixels, fully inside mask_big

        result = analysis_model.make_masks_exclusive([(mask_big, 0.9), (mask_small, 0.7)])

        assert len(result) == 1, "Small mask should be pruned (0% survival)"

    def test_preserves_non_overlapping_masks(self):
        """Non-overlapping masks are unchanged."""
        mask_left = np.zeros((10, 10), dtype=np.uint8)
        mask_left[0:5, 0:5] = 1

        mask_right = np.zeros((10, 10), dtype=np.uint8)
        mask_right[5:10, 5:10] = 1

        result = analysis_model.make_masks_exclusive([(mask_left, 0.9), (mask_right, 0.7)])

        assert len(result) == 2
        assert result[0][0].sum() == 25
        assert result[1][0].sum() == 25

    def test_empty_input_returns_empty(self):
        """Empty input list returns empty output."""
        result = analysis_model.make_masks_exclusive([])
        assert result == []

    def test_survival_threshold_boundary(self):
        """Masks with exactly 10% survival pass the threshold.

        The 10% threshold balances keeping partial masks (e.g., occluded
        buildings) vs removing noise fragments.
        """
        mask_cover = np.zeros((10, 10), dtype=np.uint8)
        mask_cover[0:9, 0:10] = 1  # 90 pixels

        mask_partial = np.zeros((10, 10), dtype=np.uint8)
        mask_partial[0:10, 0:10] = 1  # 100 pixels, 90 overlap

        result = analysis_model.make_masks_exclusive([(mask_cover, 0.9), (mask_partial, 0.7)])

        # mask_partial loses 90 pixels, keeps 10 -> 10% survival
        assert len(result) == 2, "10% survival should pass threshold"


# =============================================================================
# Containment tests (composite image detection)
# =============================================================================


class TestContainment:
    """Tests for containment detection used in composite subimage filtering.

    Containment measures how much of the smaller mask is inside the larger one.
    This is distinct from IoU: a small panel fully inside a large container has
    high containment but moderate IoU (because the union is dominated by the
    container). Composite detection uses this to filter out image-wide regions
    that SAM3 sometimes produces alongside the actual sub-images.
    """

    def test_full_containment(self):
        """Smaller mask fully inside larger returns containment ~1.0."""
        outer = np.zeros((100, 100), dtype=np.uint8)
        outer[0:80, 0:80] = 1

        inner = np.zeros((100, 100), dtype=np.uint8)
        inner[10:30, 10:30] = 1

        assert analysis_model.compute_containment(outer, inner) == pytest.approx(1.0)
        # Order shouldn't matter — containment is symmetric
        assert analysis_model.compute_containment(inner, outer) == pytest.approx(1.0)

    def test_no_overlap(self):
        """Non-overlapping masks return 0.0."""
        left = np.zeros((100, 100), dtype=np.uint8)
        left[0:50, 0:40] = 1

        right = np.zeros((100, 100), dtype=np.uint8)
        right[0:50, 60:100] = 1

        assert analysis_model.compute_containment(left, right) == 0.0

    def test_partial_overlap(self):
        """Partial overlap returns fraction of smaller mask contained."""
        mask_a = np.zeros((100, 100), dtype=np.uint8)
        mask_a[0:50, 0:50] = 1  # 2500 pixels

        mask_b = np.zeros((100, 100), dtype=np.uint8)
        mask_b[25:75, 25:75] = 1  # 2500 pixels, 625 overlap

        # Both same size, so containment = intersection / area = 625/2500 = 0.25
        assert analysis_model.compute_containment(mask_a, mask_b) == pytest.approx(0.25)

    def test_empty_masks(self):
        """Empty masks return 0.0."""
        empty = np.zeros((10, 10), dtype=np.uint8)
        nonempty = np.zeros((10, 10), dtype=np.uint8)
        nonempty[0:5, 0:5] = 1

        assert analysis_model.compute_containment(empty, nonempty) == 0.0
        assert analysis_model.compute_containment(empty, empty) == 0.0

    def test_containment_vs_iou_distinction(self):
        """Containment and IoU diverge when sizes differ significantly.

        This is the key scenario for composite detection: a large container
        mask has moderate IoU with a small panel but high containment.
        """
        # Container covers 80% of image
        container = np.zeros((100, 100), dtype=np.uint8)
        container[0:80, 0:100] = 1  # 8000 pixels

        # Panel covers 20% of image, fully inside container
        panel = np.zeros((100, 100), dtype=np.uint8)
        panel[10:30, 20:70] = 1  # 1000 pixels

        iou = analysis_model.compute_iou(container, panel)
        containment = analysis_model.compute_containment(container, panel)

        # IoU is moderate (intersection/union = 1000/8000 = 0.125)
        assert iou < 0.2
        # Containment is 1.0 (panel fully inside container)
        assert containment == pytest.approx(1.0)


# =============================================================================
# VLM prompt tests
# =============================================================================


class TestVlmPrompt:
    """Tests for VLM prompt generation."""

    def test_prompt_is_deterministic_for_kv_cache(self):
        """Same schema produces identical prompts (enables KV cache reuse)."""
        schema = make_test_schema()

        p1 = analysis_model.build_vlm_prompt(schema)
        p2 = analysis_model.build_vlm_prompt(schema)

        assert p1 == p2, "Prompt should be deterministic"

    def test_prompt_contains_required_elements(self):
        """Prompt includes instructions for region analysis and status decision."""
        schema = make_test_schema()
        prompt = analysis_model.build_vlm_prompt(schema)

        assert "circled number" in prompt, "Should reference numbered regions"
        assert "original" in prompt.lower(), "Should reference original image"
        assert "analyzed" in prompt, "Should mention analyzed status"
        assert "rejected" in prompt, "Should mention rejected status"

    def test_prompt_includes_baml_schema(self):
        """Prompt includes BAML-converted schema for clarity."""
        schema = make_test_schema()
        prompt = analysis_model.build_vlm_prompt(schema)

        assert "status" in prompt, "BAML should include status field"


# =============================================================================
# BAML converter tests
# =============================================================================


class TestBamlConverter:
    """Tests for JSON Schema to BAML conversion."""

    def test_converts_basic_types(self):
        """Converter handles string, integer, number, boolean types."""
        schema = {
            "type": "object",
            "title": "TestType",
            "properties": {
                "name": {"type": "string"},
                "count": {"type": "integer"},
                "score": {"type": "number"},
                "active": {"type": "boolean"},
            },
            "required": ["name", "count"],
        }
        baml = baml_converter.jsonschema_to_baml(schema)

        assert "name string" in baml
        assert "count int" in baml
        assert "score float?" in baml  # optional
        assert "active bool?" in baml  # optional

    def test_converts_arrays(self):
        """Converter handles array types."""
        schema = {
            "type": "object",
            "title": "ArrayTest",
            "properties": {
                "tags": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["tags"],
        }
        baml = baml_converter.jsonschema_to_baml(schema)

        assert "tags string[]" in baml

    def test_converts_nullable_types(self):
        """Converter handles nullable (union with null) types."""
        schema = {
            "type": "object",
            "title": "NullableTest",
            "properties": {
                "maybe": {"type": ["string", "null"]},
            },
            "required": ["maybe"],
        }
        baml = baml_converter.jsonschema_to_baml(schema)

        assert "maybe string?" in baml

    def test_converts_enums(self):
        """Converter handles enum definitions."""
        schema = {
            "definitions": {
                "Status": {
                    "description": "Status enum",
                    "oneOf": [
                        {"enum": ["active"], "description": "Active status"},
                        {"enum": ["inactive"], "description": "Inactive status"},
                    ],
                }
            },
        }
        baml = baml_converter.jsonschema_to_baml(schema)

        assert "enum Status" in baml
        assert "active" in baml
        assert "inactive" in baml

    def test_converts_maps(self):
        """Converter handles map types (additionalProperties)."""
        schema = {
            "type": "object",
            "title": "MapTest",
            "properties": {
                "data": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                },
            },
            "required": ["data"],
        }
        baml = baml_converter.jsonschema_to_baml(schema)

        assert "map<string, string>" in baml

    def test_converts_nested_objects_with_refs(self):
        """Converter handles nested objects via $ref."""
        schema = {
            "definitions": {
                "Inner": {
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"],
                }
            },
            "type": "object",
            "title": "Outer",
            "properties": {
                "nested": {"$ref": "#/definitions/Inner"},
            },
            "required": ["nested"],
        }
        baml = baml_converter.jsonschema_to_baml(schema)

        assert "class Inner" in baml
        assert "class Outer" in baml
        assert "nested Inner" in baml

    def test_converts_tagged_union(self):
        """Converter handles tagged unions (internally-tagged serde enums)."""
        schema = {
            "title": "TestUnion",
            "oneOf": [
                {
                    "type": "object",
                    "required": ["status", "value"],
                    "properties": {
                        "status": {"type": "string", "enum": ["ok"]},
                        "value": {"type": "integer"},
                    },
                },
                {
                    "type": "object",
                    "required": ["status", "error"],
                    "properties": {
                        "status": {"type": "string", "enum": ["error"]},
                        "error": {"type": "string"},
                    },
                },
            ],
        }
        baml = baml_converter.jsonschema_to_baml(schema)

        assert "TestUnion" in baml
        assert "status" in baml
        assert "ok" in baml
        assert "error" in baml

    def test_converts_rust_generated_schema(self):
        """Converter works with the actual Rust-generated schema.

        This ensures Python and Rust components stay in sync.
        """
        result = subprocess.run(
            ["cargo", "run", "--bin", "schematool"],
            capture_output=True,
            text=True,
            cwd=Path(__file__).parent.parent,  # analysis/ directory
        )
        if result.returncode != 0:
            pytest.skip(f"cargo not available: {result.stderr[:100]}")

        schema = json.loads(result.stdout)
        baml = baml_converter.jsonschema_to_baml(schema)

        # Check key types from simplified schema
        assert "enum RelationType" in baml
        assert "class RegionAnalysis" in baml
        assert "class Surroundings" in baml

        # Check tagged union structure
        assert "SubimageOutput" in baml
        assert "analyzed" in baml
        assert "rejected" in baml
        assert "regions RegionAnalysis[]" in baml

        # Removed types should NOT appear
        assert "SceneObservations" not in baml
        assert "RegionObservations" not in baml
        assert "ExtractedText" not in baml


# =============================================================================
# DINOv3 model tests
# =============================================================================


class TestDinov3:
    """Tests for DINOv3 embedding model preprocessing and output format."""

    def test_model_loads(self):
        """DINOv3 model module loads without errors."""
        assert hasattr(dinov3_module, "TritonPythonModel")

    def test_get_string_from_tensor(self):
        """String extraction works for the DINOv3 module's copy."""
        tensor = mock_triton.Tensor("test", np.array([b"hello"]))
        result = dinov3_module.get_string_from_tensor(tensor)
        assert result == "hello"


# =============================================================================
# Integration tests
# =============================================================================


def _make_sam3_handler():
    """Create a mock SAM3 handler that returns valid regions.

    Uses the module-level sam3_module for encode_rle.
    """

    def handler(inputs):
        image_tensor = inputs.get("image")
        assert image_tensor is not None, "SAM3 should receive image input"

        # SAM3 has max_batch_size=1, so expects 2D input [batch, data]
        shape = image_tensor.as_numpy().shape
        assert len(shape) == 2, f"SAM3 expects 2D input, got shape {shape}"

        img_b64 = image_tensor.as_numpy().flatten()[0].decode("utf-8")
        img_bytes = base64.b64decode(img_b64)
        img = Image.open(io.BytesIO(img_bytes))

        # Create a mock region (0-indexed, no region_id)
        mask = np.zeros((img.height, img.width), dtype=np.uint8)
        mask[10:50, 10:50] = 1
        regions = [{"confidence": 0.85, "mask": sam3_module.encode_rle(mask)}]

        return mock_triton.InferenceResponse(
            [mock_triton.Tensor("regions", np.array([json.dumps(regions).encode("utf-8")]))]
        )

    return handler


def _make_subimage_fallback_sam3():
    """Create a SAM3 handler: empty for subimage, valid for entity.

    Subimage detection finds nothing (falls back to single full image),
    but entity segmentation returns regions.
    """
    entity_handler = _make_sam3_handler()

    def handler(inputs):
        prompts = inputs.get("prompts")
        if prompts is not None:
            # Subimage detection — return empty to trigger full-image fallback
            return mock_triton.InferenceResponse(
                [mock_triton.Tensor("regions", np.array([json.dumps([]).encode("utf-8")]))]
            )
        # Entity segmentation
        return entity_handler(inputs)

    return handler


def _make_vlm_response(**overrides: object) -> dict:
    """Build a valid VLM analyzed response with optional field overrides.

    Returns a canonical vlm_schema::SubimageOutput (analyzed variant).
    Callers override specific fields via keyword args (single-level merge for dicts).
    """
    base = {
        "status": "analyzed",
        "scene": {
            "media_type": {"type": "photo", "color": "color"},
            "content_summary": "Test building",
            "scene_type": "outdoor",
        },
        "regions": [
            {
                "entity_type": "building",
                "description": "A test building",
                "surroundings": {
                    "non_entity": [],
                    "other_non_entity": [],
                    "related_regions": [],
                },
            }
        ],
    }
    result: dict = copy.deepcopy(base)
    for key, value in overrides.items():
        if isinstance(value, dict) and isinstance(result.get(key), dict):
            result[key].update(value)
        else:
            result[key] = value
    return result


def _make_vlm_handler(**overrides: object):
    """Create a mock VLM handler that returns analyzed vlm_schema::SubimageOutput."""

    def handler(inputs):
        text_input = inputs.get("text_input")
        image_input = inputs.get("image")
        sampling_params = inputs.get("sampling_parameters")

        assert text_input is not None, "VLM should receive text_input"
        assert image_input is not None, "VLM should receive image"
        assert sampling_params is not None, "VLM should receive sampling_parameters"

        # VLM has max_batch_size=0, so expects 1D input [data]
        assert text_input.as_numpy().ndim == 1, "VLM expects 1D text input"
        assert sampling_params.as_numpy().ndim == 1, "VLM expects 1D sampling params"

        # Verify ChatML prompt structure
        prompt = text_input.as_numpy().flatten()[0].decode("utf-8")
        assert "<|im_start|>user" in prompt, "Should use ChatML format"
        assert "<|vision_start|>" in prompt, "Should have vision placeholders"

        # Verify images are provided as separate array elements
        images = image_input.as_numpy()
        assert len(images) == 2, f"Should have 2 images, got {len(images)}"

        # Verify structured_outputs is set
        params = json.loads(sampling_params.as_numpy().flatten()[0].decode("utf-8"))
        assert "structured_outputs" in params

        result = _make_vlm_response(**overrides)
        return mock_triton.InferenceResponse(
            [mock_triton.Tensor("text_output", np.array([json.dumps(result).encode("utf-8")]))]
        )

    return handler


def _make_rejected_vlm_handler(reason: str = "No structures detected"):
    """Create a mock VLM handler that returns rejected vlm_schema::SubimageOutput."""

    def handler(inputs):
        result = {"status": "rejected", "reason": reason}
        return mock_triton.InferenceResponse(
            [mock_triton.Tensor("text_output", np.array([json.dumps(result).encode("utf-8")]))]
        )

    return handler


def _make_dinov3_handler():
    """Create a mock DINOv3 handler that returns fake embeddings."""

    def handler(inputs):
        images_tensor = inputs.get("images")
        assert images_tensor is not None, "DINOv3 should receive images input"

        num_images = len(images_tensor.as_numpy().flatten())

        # Return fake 1024-dim L2-normalized embeddings
        embeddings = []
        for i in range(num_images):
            emb = [0.0] * 1024
            emb[i % 1024] = 1.0  # Unit vector for easy verification
            embeddings.append(emb)

        return mock_triton.InferenceResponse(
            [mock_triton.Tensor("embeddings", np.array([json.dumps(embeddings).encode("utf-8")]))]
        )

    return handler


def _parse_result(response: mock_triton.InferenceResponse) -> dict[str, Any]:
    """Extract and parse the 'result' JSON from an inference response."""
    tensor = mock_triton.get_output_tensor_by_name(response, "result")
    assert tensor is not None, "missing 'result' output tensor"
    result: dict[str, Any] = json.loads(tensor.as_numpy().flatten()[0].decode("utf-8"))
    return result


class TestAnalysisOrchestration:
    """Integration tests for the subimage-centric analysis pipeline."""

    def test_single_image_pipeline(self):
        """Full pipeline with single image: SAM3 subimage detection finds nothing -> full image."""
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3())
        mock_triton.register_model("vlm", _make_vlm_handler())
        mock_triton.register_model("dinov3", _make_dinov3_handler())

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        responses = model.execute([request])

        assert len(responses) == 1
        response = responses[0]
        assert not response.has_error()

        result = _parse_result(response)

        # Verify top-level AnalysisResult tagged enum
        assert result["outcome"] == "success"
        assert "subimages" in result
        assert len(result["subimages"]) == 1

        subimage = result["subimages"][0]
        assert "bounds" in subimage
        assert "analysis" in subimage

        # Bounds should cover the full image
        bounds = subimage["bounds"]
        assert bounds["bbox"]["x"] == 0
        assert bounds["bbox"]["y"] == 0
        assert bounds["bbox"]["width"] == 200
        assert bounds["bbox"]["height"] == 150

        # Analysis should be "analyzed"
        analysis = subimage["analysis"]
        assert analysis["status"] == "analyzed"
        assert analysis["scene"]["content_summary"] == "Test building"

        # Should have regions from entity SAM3 (0-indexed, no region_id)
        assert len(analysis["regions"]) == 1
        region = analysis["regions"][0]
        assert region["analysis"]["entity_type"] == "building"
        assert region["mask"]["counts"] != ""  # Should have RLE mask
        assert "region_id" not in region

        # Should have embeddings
        assert len(analysis["embedding"]) == 1024
        assert len(region["embedding"]) == 1024

        # Should have versions
        assert "versions" in result
        assert "vlm" in result["versions"]
        assert "sam3" in result["versions"]
        assert "dinov3" in result["versions"]
        assert "git_sha" in result["versions"]

    def test_rejected_subimage(self):
        """VLM rejects a subimage as not relevant."""
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3())
        mock_triton.register_model("vlm", _make_rejected_vlm_handler("portrait photo"))
        mock_triton.register_model("dinov3", _make_dinov3_handler())

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        responses = model.execute([request])
        result = _parse_result(responses[0])

        assert len(result["subimages"]) == 1
        analysis = result["subimages"][0]["analysis"]
        assert analysis["status"] == "rejected"
        assert analysis["reason"] == "portrait photo"

    def test_sam3_error_fails_pipeline(self):
        """SAM3 errors propagate — no silent fallback."""

        def failing_sam3(inputs):
            return mock_triton.InferenceResponse(
                error=mock_triton.TritonError("SAM3 out of memory")
            )

        mock_triton.register_model("sam3", failing_sam3)
        mock_triton.register_model("vlm", _make_vlm_handler())
        mock_triton.register_model("dinov3", _make_dinov3_handler())

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        with pytest.raises(RuntimeError, match="SAM3"):
            model.execute([request])

    def test_handles_vlm_error_gracefully(self):
        """Pipeline handles VLM errors — result is error status."""
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3())
        mock_triton.register_model("dinov3", _make_dinov3_handler())

        def failing_vlm(inputs):
            return mock_triton.InferenceResponse(error=mock_triton.TritonError("VLM out of memory"))

        mock_triton.register_model("vlm", failing_vlm)

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        responses = model.execute([request])
        result = _parse_result(responses[0])

        analysis = result["subimages"][0]["analysis"]
        assert analysis["status"] == "error"
        assert "VLM" in analysis["message"]

    def test_rejects_oversized_images(self):
        """Pipeline rejects images exceeding size limit (5MB)."""
        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        # Create a "large" image by making a long base64 string
        fake_large_b64 = "A" * (7 * 1024 * 1024)

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([fake_large_b64.encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        responses = model.execute([request])
        result = _parse_result(responses[0])

        assert result["outcome"] == "image_rejected"
        assert "too large" in result["reason"]

    def test_handles_zero_regions_from_sam3(self):
        """Pipeline works when entity SAM3 finds no regions."""

        def subimage_sam3(inputs):
            # Both subimage detection and entity segmentation return empty
            return mock_triton.InferenceResponse(
                [mock_triton.Tensor("regions", np.array([json.dumps([]).encode("utf-8")]))]
            )

        mock_triton.register_model("sam3", subimage_sam3)
        mock_triton.register_model("vlm", _make_vlm_handler(regions=[]))
        mock_triton.register_model("dinov3", _make_dinov3_handler())

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        responses = model.execute([request])
        result = _parse_result(responses[0])

        assert len(result["subimages"]) == 1
        analysis = result["subimages"][0]["analysis"]
        assert analysis["status"] == "analyzed"
        assert analysis["regions"] == []

    def test_handles_vlm_malformed_json(self):
        """Malformed VLM JSON surfaces as per-subimage error, not pipeline crash."""
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3())
        mock_triton.register_model("dinov3", _make_dinov3_handler())

        def bad_json_vlm(inputs):
            return mock_triton.InferenceResponse(
                [mock_triton.Tensor("text_output", np.array([b"not valid json {{{"]))]
            )

        mock_triton.register_model("vlm", bad_json_vlm)

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        responses = model.execute([request])
        result = json.loads(responses[0].output_tensors()[0].as_numpy().item().decode("utf-8"))
        # VLM error should produce per-subimage error, not crash the pipeline
        analysis = result["subimages"][0]["analysis"]
        assert analysis["status"] == "error"
        assert "VLM call failed" in analysis["message"]

    def test_dinov3_error_preserves_vlm_results(self):
        """DINOv3 errors are non-fatal — VLM results are preserved without embeddings."""
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3())
        mock_triton.register_model("vlm", _make_vlm_handler())

        def failing_dinov3(inputs):
            return mock_triton.InferenceResponse(
                error=mock_triton.TritonError("DINOv3 out of memory")
            )

        mock_triton.register_model("dinov3", failing_dinov3)

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        # DINOv3 failure is non-fatal — results come back without embeddings
        responses = model.execute([request])
        result = _parse_result(responses[0])

        analysis = result["subimages"][0]["analysis"]
        assert analysis["status"] == "analyzed", "VLM results should be preserved"
        assert "embedding" not in analysis, "subimage embedding should be absent"
        assert "embedding" not in analysis["regions"][0], "region embedding should be absent"

    def test_subimage_detection_uses_prompt(self):
        """Subimage detection calls SAM3 with panel detection prompts."""
        received_prompts = []
        entity_handler = _make_sam3_handler()

        def tracking_sam3(inputs):
            prompts = inputs.get("prompts")
            if prompts is not None:
                received_prompts.extend([p.decode("utf-8") for p in prompts.as_numpy().flatten()])
                # Return empty to trigger fallback
                return mock_triton.InferenceResponse(
                    [mock_triton.Tensor("regions", np.array([json.dumps([]).encode("utf-8")]))]
                )
            return entity_handler(inputs)

        mock_triton.register_model("sam3", tracking_sam3)
        mock_triton.register_model("vlm", _make_vlm_handler())
        mock_triton.register_model("dinov3", _make_dinov3_handler())

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        model.execute([request])

        # Should have received subimage detection prompt for collage detection
        assert len(received_prompts) == 1
        assert "collage" in received_prompts[0]

    def test_container_region_filtered_from_composite(self):
        """Image-wide container region is removed, keeping individual sub-images.

        SAM3 sometimes produces a high-confidence region spanning the entire
        composite alongside the actual sub-image panels. Containment filtering
        identifies and removes the container so the panels are preserved for
        independent analysis. This is different from entity detection, which
        uses exclusive pixel claiming (where the container would eat the panels).
        """
        entity_handler = _make_sam3_handler()
        # Image is 200x150 (from make_test_image)
        width, height = 200, 150

        def composite_sam3(inputs):
            prompts = inputs.get("prompts")
            if prompts is not None:
                # Subimage detection: container + two panels
                container = np.zeros((height, width), dtype=np.uint8)
                container[5:145, 5:195] = 1  # ~90% of image

                left_panel = np.zeros((height, width), dtype=np.uint8)
                left_panel[10:140, 10:95] = 1  # left half

                right_panel = np.zeros((height, width), dtype=np.uint8)
                right_panel[10:140, 105:190] = 1  # right half

                regions = [
                    {"confidence": 0.95, "mask": sam3_module.encode_rle(container)},
                    {"confidence": 0.85, "mask": sam3_module.encode_rle(left_panel)},
                    {"confidence": 0.80, "mask": sam3_module.encode_rle(right_panel)},
                ]
                return mock_triton.InferenceResponse(
                    [mock_triton.Tensor("regions", np.array([json.dumps(regions).encode("utf-8")]))]
                )
            # Entity segmentation
            return entity_handler(inputs)

        mock_triton.register_model("sam3", composite_sam3)
        mock_triton.register_model("vlm", _make_vlm_handler())
        mock_triton.register_model("dinov3", _make_dinov3_handler())

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        responses = model.execute([request])
        result = _parse_result(responses[0])

        # Container should be filtered out, leaving two sub-images
        assert len(result["subimages"]) == 2
        for subimage in result["subimages"]:
            assert subimage["analysis"]["status"] == "analyzed"

    def test_no_subimage_regions_produces_single_subimage(self):
        """When SAM3 finds no composite panels, the full image is analyzed as one subimage.

        This is the expected behavior for non-composite images (single photos)
        and also the fallback when SAM3 fails to detect any panels.
        """

        # SAM3 returns empty for both subimage and entity detection
        def empty_sam3(inputs):
            return mock_triton.InferenceResponse(
                [mock_triton.Tensor("regions", np.array([json.dumps([]).encode("utf-8")]))]
            )

        mock_triton.register_model("sam3", empty_sam3)
        mock_triton.register_model("vlm", _make_vlm_handler(regions=[]))
        mock_triton.register_model("dinov3", _make_dinov3_handler())

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        responses = model.execute([request])
        result = _parse_result(responses[0])

        # Should produce exactly one subimage covering the full image
        assert len(result["subimages"]) == 1
        bounds = result["subimages"][0]["bounds"]
        assert bounds["bbox"]["x"] == 0
        assert bounds["bbox"]["y"] == 0
        assert bounds["bbox"]["width"] == 200
        assert bounds["bbox"]["height"] == 150
        assert result["subimages"][0]["analysis"]["status"] == "analyzed"


# =============================================================================
# Schema compatibility tests
# =============================================================================


class TestSchemaCompatibility:
    """Tests to ensure Python output matches Rust schema expectations.

    These tests validate that the JSON produced by the Python models can be
    deserialized by the Rust types, catching schema drift early.
    """

    def test_analysis_result_matches_rust_schema(self):
        """Full pipeline output validates against Rust AnalysisResult schema."""
        import jsonschema

        # Get the Rust-generated schema
        result = subprocess.run(
            ["cargo", "run", "--bin", "schematool", "result"],
            capture_output=True,
            text=True,
            cwd=Path(__file__).parent.parent,
        )
        if result.returncode != 0:
            pytest.skip(f"cargo not available: {result.stderr[:100]}")

        rust_schema = json.loads(result.stdout)

        # Set up pipeline with subimage detection fallback
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3())

        mock_triton.register_model(
            "vlm",
            _make_vlm_handler(
                scene={
                    "media_type": {"type": "photo", "color": "monochrome"},
                    "content_summary": "A building",
                    "scene_type": "outdoor",
                },
                regions=[
                    {
                        "entity_type": "building",
                        "description": "A brick building",
                        "surroundings": {
                            "non_entity": [],
                            "other_non_entity": [],
                            "related_regions": [],
                        },
                    }
                ],
            ),
        )
        mock_triton.register_model("dinov3", _make_dinov3_handler())

        model = analysis_model.TritonPythonModel()
        model.initialize({"model_config": json.dumps({})})

        request = mock_triton.InferenceRequest(
            model_name="analysis",
            requested_output_names=["result"],
            inputs=[
                mock_triton.Tensor("image", np.array([make_test_image().encode("utf-8")])),
                mock_triton.Tensor("schema", np.array([make_test_schema().encode("utf-8")])),
            ],
        )

        responses = model.execute([request])
        python_output = _parse_result(responses[0])

        # Validate against Rust schema
        jsonschema.validate(instance=python_output, schema=rust_schema)

        # Validate via serde deserialization (catches deny_unknown_fields, tagging, etc.)
        validate_result = subprocess.run(
            ["cargo", "run", "--bin", "schematool", "--", "validate"],
            input=json.dumps(python_output),
            capture_output=True,
            text=True,
            cwd=Path(__file__).parent.parent,
        )
        assert validate_result.returncode == 0, f"serde validation failed: {validate_result.stderr}"

        # Verify subimage structure
        assert "subimages" in python_output
        assert len(python_output["subimages"]) == 1
        subimage = python_output["subimages"][0]
        assert "bounds" in subimage
        assert "analysis" in subimage
        assert subimage["analysis"]["status"] == "analyzed"
        assert len(subimage["analysis"]["regions"]) == 1
        assert len(subimage["analysis"]["embedding"]) == 1024


# =============================================================================
# Real model integration tests
# =============================================================================


def _make_pipeline_request(image_b64: str) -> mock_triton.InferenceRequest:
    """Create a pipeline request with VLM skipped (for real-model integration tests)."""
    return mock_triton.InferenceRequest(
        model_name="analysis",
        requested_output_names=["result"],
        inputs=[
            mock_triton.Tensor("image", np.array([[image_b64.encode("utf-8")]])),
            mock_triton.Tensor("schema", np.array([[b"{}"]])),
            mock_triton.Tensor("skip_vlm", np.array([[True]])),
        ],
    )


class TestPipelineIntegration:
    """Integration tests using real SAM3/DINOv3 models (VLM skipped).

    These exercise the full pipeline with actual model inference on MPS/CPU.
    """

    def test_segmented_output_structure(self, pipeline):
        """Real pipeline produces valid Segmented output for a solid-color image."""
        image_b64 = make_test_image()

        responses = pipeline.execute([_make_pipeline_request(image_b64)])
        result = _parse_result(responses[0])

        assert result["outcome"] == "success"
        assert len(result["subimages"]) >= 1

        for subimage in result["subimages"]:
            assert "bounds" in subimage
            analysis = subimage["analysis"]
            # VLM skipped -> Segmented variant
            assert analysis["status"] == "segmented"
            assert isinstance(analysis["regions"], list)
            # Subimage embedding should be 1024-dim
            assert len(analysis["embedding"]) == 1024
            assert abs(sum(x**2 for x in analysis["embedding"]) - 1.0) < 0.01

    def test_embeddings_are_deterministic(self, pipeline):
        """Same image produces identical embeddings across runs."""
        image_b64 = make_test_image(width=100, height=100)

        def run_once() -> list[float]:
            responses = pipeline.execute([_make_pipeline_request(image_b64)])
            result = _parse_result(responses[0])
            embedding: list[float] = result["subimages"][0]["analysis"]["embedding"]
            return embedding

        emb1 = run_once()
        emb2 = run_once()
        diff = sum((a - b) ** 2 for a, b in zip(emb1, emb2, strict=True)) ** 0.5
        assert diff < 1e-5, f"Embeddings differ by L2={diff}"

    def test_different_images_produce_different_embeddings(self, pipeline):
        """Distinct images produce meaningfully different embeddings."""
        img_a = make_test_image(width=200, height=200)
        # Create a visually different image
        img_bright = Image.new("RGB", (200, 200), color=(255, 0, 0))
        buffer = io.BytesIO()
        img_bright.save(buffer, format="JPEG", quality=85)
        img_b = base64.b64encode(buffer.getvalue()).decode("utf-8")

        def get_embedding(b64: str) -> list[float]:
            responses = pipeline.execute([_make_pipeline_request(b64)])
            result = _parse_result(responses[0])
            embedding: list[float] = result["subimages"][0]["analysis"]["embedding"]
            return embedding

        emb_a = get_embedding(img_a)
        emb_b = get_embedding(img_b)

        cosine_sim = sum(a * b for a, b in zip(emb_a, emb_b, strict=True))
        assert cosine_sim < 0.99, f"Distinct images too similar: cosine={cosine_sim:.4f}"

    def test_schema_validation_with_real_output(self, pipeline):
        """Real pipeline output validates against the Rust AnalysisResult schema."""
        result_proc = subprocess.run(
            ["cargo", "run", "--bin", "schematool", "--", "validate"],
            input="null",
            capture_output=True,
            text=True,
            cwd=Path(__file__).parent.parent,
        )
        if result_proc.returncode != 0 and "cargo" in result_proc.stderr.lower():
            pytest.skip("cargo not available for schema validation")

        image_b64 = make_test_image()

        responses = pipeline.execute([_make_pipeline_request(image_b64)])
        result = _parse_result(responses[0])

        validate_result = subprocess.run(
            ["cargo", "run", "--bin", "schematool", "--", "validate"],
            input=json.dumps(result),
            capture_output=True,
            text=True,
            cwd=Path(__file__).parent.parent,
        )
        assert validate_result.returncode == 0, f"serde validation failed: {validate_result.stderr}"
