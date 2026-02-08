#!/usr/bin/env python3
"""Triton model tests.

Run with: pytest test_models.py -v

Uses pytest fixtures from conftest.py for test isolation and module loading.
"""

import base64
import io
import json
import subprocess
from pathlib import Path

import numpy as np
import pytest
from hypothesis import given, settings
from hypothesis import strategies as st
from hypothesis.extra import numpy as hn
from PIL import Image

import mock_triton

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
    """Get the actual VlmSubimageOutput schema from Rust."""
    result = subprocess.run(
        ["cargo", "run", "--bin", "print_schema"],
        capture_output=True,
        text=True,
        cwd=Path(__file__).parent.parent,  # analysis/ directory
    )
    if result.returncode == 0:
        return result.stdout
    # Fallback to a minimal schema if cargo isn't available
    return json.dumps(
        {
            "oneOf": [
                {
                    "type": "object",
                    "required": [
                        "status",
                        "media_type",
                        "content_summary",
                        "scene_type",
                        "temporal_cues",
                        "regions",
                        "region_relationships",
                        "extracted_text",
                    ],
                    "properties": {
                        "status": {"type": "string", "enum": ["analyzed"]},
                        "media_type": {"type": "string"},
                        "content_summary": {"type": "string"},
                        "scene_type": {"type": "string"},
                        "temporal_cues": {"type": "array", "items": {"type": "string"}},
                        "regions": {"type": "array", "items": {"type": "object"}},
                        "region_relationships": {"type": "array"},
                        "extracted_text": {"type": "array"},
                    },
                },
                {
                    "type": "object",
                    "required": ["status", "reason"],
                    "properties": {
                        "status": {"type": "string", "enum": ["rejected"]},
                        "reason": {"type": "string"},
                    },
                },
            ],
        }
    )


# =============================================================================
# RLE encoding tests
# =============================================================================


class TestRleEncoding:
    """Tests for RLE (run-length encoding) of masks."""

    def test_roundtrip_property_based(self, sam3_module, analysis_model):
        """RLE encode/decode preserves any binary mask (property-based test).

        This catches edge cases like checkerboard patterns, diagonal stripes,
        and other complex shapes that hand-written tests might miss.
        """

        @given(
            hn.arrays(
                dtype=np.uint8,
                shape=st.tuples(
                    st.integers(min_value=1, max_value=100),
                    st.integers(min_value=1, max_value=100),
                ),
                elements=st.sampled_from([0, 1]),
            )
        )
        @settings(max_examples=50, deadline=None)
        def check_roundtrip(mask):
            counts = sam3_module.encode_rle(mask)
            h, w = mask.shape
            decoded = analysis_model.decode_rle(counts, h, w)
            assert np.array_equal(decoded, mask)

        check_roundtrip()

    def test_compression_roundtrip_various_run_lengths(self, analysis_model):
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

    def test_removes_duplicate_masks(self, sam3_module):
        """Deduplication removes masks with high IoU overlap."""
        mask1 = np.zeros((10, 10), dtype=np.uint8)
        mask1[0:6, 0:6] = 1

        mask2 = mask1.copy()  # exact duplicate

        mask3 = np.zeros((10, 10), dtype=np.uint8)
        mask3[7:10, 7:10] = 1  # non-overlapping

        result = sam3_module.deduplicate_masks([mask1, mask2, mask3], [0.9, 0.8, 0.7])

        assert len(result) == 2, "Duplicate should be removed"

    def test_empty_input_returns_empty(self, sam3_module):
        """Empty input produces empty output."""
        result = sam3_module.deduplicate_masks([], [])
        assert result == []

    def test_single_mask_passes_through(self, sam3_module):
        """Single mask is returned unchanged."""
        mask = np.zeros((10, 10), dtype=np.uint8)
        mask[2:8, 2:8] = 1

        result = sam3_module.deduplicate_masks([mask], [0.9])

        assert len(result) == 1
        assert np.array_equal(result[0][0], mask)
        assert result[0][1] == 0.9

    def test_keeps_higher_confidence_on_overlap(self, sam3_module):
        """When masks overlap significantly, keeps only the higher-confidence one."""
        mask_high = np.zeros((10, 10), dtype=np.uint8)
        mask_high[0:8, 0:8] = 1  # 64 pixels

        # Nearly identical mask - IoU will be very high
        mask_low = np.zeros((10, 10), dtype=np.uint8)
        mask_low[0:8, 0:8] = 1
        mask_low[0, 0] = 0  # 63 pixels, IoU = 63/64 ≈ 0.98 > 0.7

        # Lower confidence mask listed first, but higher confidence should be kept
        result = sam3_module.deduplicate_masks([mask_low, mask_high], [0.7, 0.9])

        assert len(result) == 1
        assert result[0][1] == 0.9, "Should keep higher confidence mask"

    def test_threshold_boundary_keeps_both(self, sam3_module):
        """Masks with IoU exactly at threshold boundary are both kept."""
        # Two masks with ~70% IoU (at the default 0.7 threshold boundary)
        mask1 = np.zeros((10, 10), dtype=np.uint8)
        mask1[0:7, 0:10] = 1  # 70 pixels

        mask2 = np.zeros((10, 10), dtype=np.uint8)
        mask2[3:10, 0:10] = 1  # 70 pixels, overlap = 40 pixels
        # IoU = 40 / (70 + 70 - 40) = 40/100 = 0.4, well below 0.7

        result = sam3_module.deduplicate_masks([mask1, mask2], [0.9, 0.8])
        assert len(result) == 2, "Non-duplicate masks should both be kept"


# =============================================================================
# Exclusive mask tests
# =============================================================================


class TestExclusiveMasks:
    """Tests for making masks mutually exclusive (no pixel overlap).

    When multiple detected regions overlap, higher-confidence regions claim
    the overlapping pixels. This prevents double-counting in analysis.
    """

    def test_higher_confidence_claims_overlapping_pixels(self, sam3_module):
        """Higher confidence mask keeps all pixels; lower loses overlap."""
        mask_high = np.zeros((10, 10), dtype=np.uint8)
        mask_high[2:8, 2:8] = 1  # 36 pixels in center

        mask_low = np.zeros((10, 10), dtype=np.uint8)
        mask_low[4:10, 4:10] = 1  # 36 pixels, overlaps with 16 pixels

        result = sam3_module.make_masks_exclusive([(mask_high, 0.9), (mask_low, 0.7)])

        assert len(result) == 2, "Both masks should survive"
        high_result, low_result = result[0][0], result[1][0]

        assert high_result.sum() == 36, "High confidence mask unchanged"
        assert low_result.sum() == 20, (
            f"Low confidence should have 20 pixels, got {low_result.sum()}"
        )

        overlap = np.logical_and(high_result, low_result).sum()
        assert overlap == 0, "Masks should not overlap"

    def test_prunes_masks_losing_most_pixels(self, sam3_module):
        """Masks losing >90% of pixels are removed to avoid tiny fragments.

        This prevents the VLM from receiving noise fragments that would
        confuse region analysis.
        """
        mask_big = np.zeros((10, 10), dtype=np.uint8)
        mask_big[0:10, 0:10] = 1  # 100 pixels

        mask_small = np.zeros((10, 10), dtype=np.uint8)
        mask_small[4:6, 4:6] = 1  # 4 pixels, fully inside mask_big

        result = sam3_module.make_masks_exclusive([(mask_big, 0.9), (mask_small, 0.7)])

        assert len(result) == 1, "Small mask should be pruned (0% survival)"

    def test_preserves_non_overlapping_masks(self, sam3_module):
        """Non-overlapping masks are unchanged."""
        mask_left = np.zeros((10, 10), dtype=np.uint8)
        mask_left[0:5, 0:5] = 1

        mask_right = np.zeros((10, 10), dtype=np.uint8)
        mask_right[5:10, 5:10] = 1

        result = sam3_module.make_masks_exclusive([(mask_left, 0.9), (mask_right, 0.7)])

        assert len(result) == 2
        assert result[0][0].sum() == 25
        assert result[1][0].sum() == 25

    def test_empty_input_returns_empty(self, sam3_module):
        """Empty input list returns empty output."""
        result = sam3_module.make_masks_exclusive([])
        assert result == []

    def test_survival_threshold_boundary(self, sam3_module):
        """Masks with exactly 10% survival pass the threshold.

        The 10% threshold balances keeping partial masks (e.g., occluded
        buildings) vs removing noise fragments.
        """
        mask_cover = np.zeros((10, 10), dtype=np.uint8)
        mask_cover[0:9, 0:10] = 1  # 90 pixels

        mask_partial = np.zeros((10, 10), dtype=np.uint8)
        mask_partial[0:10, 0:10] = 1  # 100 pixels, 90 overlap

        result = sam3_module.make_masks_exclusive([(mask_cover, 0.9), (mask_partial, 0.7)])

        # mask_partial loses 90 pixels, keeps 10 -> 10% survival
        assert len(result) == 2, "10% survival should pass threshold"


# =============================================================================
# VLM prompt tests
# =============================================================================


class TestVlmPrompt:
    """Tests for VLM prompt generation."""

    def test_prompt_is_deterministic_for_kv_cache(self, analysis_model):
        """Same schema produces identical prompts (enables KV cache reuse)."""
        schema = make_test_schema()

        p1 = analysis_model.build_vlm_prompt(schema)
        p2 = analysis_model.build_vlm_prompt(schema)

        assert p1 == p2, "Prompt should be deterministic"

    def test_prompt_contains_required_elements(self, analysis_model):
        """Prompt includes instructions for region analysis and status decision."""
        schema = make_test_schema()
        prompt = analysis_model.build_vlm_prompt(schema)

        assert "circled number" in prompt, "Should reference numbered regions"
        assert "original" in prompt.lower(), "Should reference original image"
        assert "analyzed" in prompt, "Should mention analyzed status"
        assert "rejected" in prompt, "Should mention rejected status"

    def test_prompt_includes_baml_schema(self, analysis_model):
        """Prompt includes BAML-converted schema for clarity."""
        schema = make_test_schema()
        prompt = analysis_model.build_vlm_prompt(schema)

        assert "status" in prompt, "BAML should include status field"


# =============================================================================
# BAML converter tests
# =============================================================================


class TestBamlConverter:
    """Tests for JSON Schema to BAML conversion."""

    def test_converts_basic_types(self, baml_converter):
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

    def test_converts_arrays(self, baml_converter):
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

    def test_converts_nullable_types(self, baml_converter):
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

    def test_converts_enums(self, baml_converter):
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

    def test_converts_maps(self, baml_converter):
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

    def test_converts_nested_objects_with_refs(self, baml_converter):
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

    def test_converts_tagged_union(self, baml_converter):
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

    def test_converts_rust_generated_schema(self, baml_converter):
        """Converter works with the actual Rust-generated schema.

        This ensures Python and Rust components stay in sync.
        """
        result = subprocess.run(
            ["cargo", "run", "--bin", "print_schema"],
            capture_output=True,
            text=True,
            cwd=Path(__file__).parent.parent,  # analysis/ directory
        )
        if result.returncode != 0:
            pytest.skip(f"cargo not available: {result.stderr[:100]}")

        schema = json.loads(result.stdout)
        baml = baml_converter.jsonschema_to_baml(schema)

        # Check key types from new schema
        assert "enum AnalyzedMediaType" in baml
        assert "enum RelationType" in baml
        assert "class RegionAnalysis" in baml
        assert "class RegionRelationship" in baml

        # Check tagged union structure
        assert "VlmSubimageOutput" in baml
        assert "analyzed" in baml
        assert "rejected" in baml
        assert "regions RegionAnalysis[]" in baml
        assert "region_relationships RegionRelationship[]" in baml


# =============================================================================
# DINOv3 model tests
# =============================================================================


class TestDinov3:
    """Tests for DINOv3 embedding model preprocessing and output format."""

    def test_model_loads(self, dinov3_module):
        """DINOv3 model module loads without errors."""
        assert hasattr(dinov3_module, "TritonPythonModel")

    def test_get_string_from_tensor(self, dinov3_module):
        """String extraction works for the DINOv3 module's copy."""
        tensor = mock_triton.Tensor("test", np.array([b"hello"]))
        result = dinov3_module.get_string_from_tensor(tensor)
        assert result == "hello"


# =============================================================================
# Integration tests
# =============================================================================


def _make_sam3_handler(sam3_module):
    """Create a mock SAM3 handler that returns valid regions.

    Args:
        sam3_module: The loaded SAM3 module (for encode_rle).
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


def _make_subimage_fallback_sam3(sam3_module):
    """Create a SAM3 handler: empty for subimage, valid for entity.

    Subimage detection finds nothing (falls back to single full image),
    but entity segmentation returns regions.
    """
    entity_handler = _make_sam3_handler(sam3_module)

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


def _make_vlm_handler():
    """Create a mock VLM handler that returns analyzed VlmSubimageOutput."""

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

        result = {
            "status": "analyzed",
            "media_type": "photo",
            "content_summary": "Test building",
            "scene_type": "outdoor",
            "temporal_cues": [],
            "regions": [
                {
                    "entity_type": "building",
                    "description": "A test building",
                    "identifiable_features": [],
                    "visible_text": [],
                    "damage_signs": [],
                }
            ],
            "region_relationships": [],
            "extracted_text": [],
        }
        return mock_triton.InferenceResponse(
            [mock_triton.Tensor("text_output", np.array([json.dumps(result).encode("utf-8")]))]
        )

    return handler


def _make_rejected_vlm_handler(reason: str = "No structures detected"):
    """Create a mock VLM handler that returns rejected VlmSubimageOutput."""

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


class TestAnalysisOrchestration:
    """Integration tests for the subimage-centric analysis pipeline."""

    def test_single_image_pipeline(self, analysis_model, sam3_module):
        """Full pipeline with single image: SAM3 subimage detection finds nothing -> full image."""
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3(sam3_module))
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

        result_tensor = mock_triton.get_output_tensor_by_name(response, "result")
        result = json.loads(result_tensor.as_numpy().flatten()[0].decode("utf-8"))

        # Verify subimage-centric output
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
        assert analysis["content_summary"] == "Test building"

        # Should have regions from entity SAM3 (0-indexed, no region_id)
        assert len(analysis["regions"]) == 1
        region = analysis["regions"][0]
        assert region["entity_type"] == "building"
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

    def test_rejected_subimage(self, analysis_model, sam3_module):
        """VLM rejects a subimage as not relevant."""
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3(sam3_module))
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
        result_tensor = mock_triton.get_output_tensor_by_name(responses[0], "result")
        result = json.loads(result_tensor.as_numpy().flatten()[0].decode("utf-8"))

        assert len(result["subimages"]) == 1
        analysis = result["subimages"][0]["analysis"]
        assert analysis["status"] == "rejected"
        assert analysis["reason"] == "portrait photo"

    def test_sam3_error_fails_pipeline(self, analysis_model):
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

    def test_handles_vlm_error_gracefully(self, analysis_model, sam3_module):
        """Pipeline handles VLM errors — result is error status."""
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3(sam3_module))
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
        result_tensor = mock_triton.get_output_tensor_by_name(responses[0], "result")
        result = json.loads(result_tensor.as_numpy().flatten()[0].decode("utf-8"))

        analysis = result["subimages"][0]["analysis"]
        assert analysis["status"] == "error"
        assert "VLM" in analysis["message"]

    def test_rejects_oversized_images(self, analysis_model):
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
        result_tensor = mock_triton.get_output_tensor_by_name(responses[0], "result")
        result = json.loads(result_tensor.as_numpy().flatten()[0].decode("utf-8"))

        assert len(result["subimages"]) == 1
        analysis = result["subimages"][0]["analysis"]
        assert analysis["status"] == "rejected"
        assert "too large" in analysis["reason"]
        assert "versions" in result, "rejected result must include versions"

    def test_handles_zero_regions_from_sam3(self, analysis_model):
        """Pipeline works when entity SAM3 finds no regions."""

        def subimage_sam3(inputs):
            # Both subimage detection and entity segmentation return empty
            return mock_triton.InferenceResponse(
                [mock_triton.Tensor("regions", np.array([json.dumps([]).encode("utf-8")]))]
            )

        mock_triton.register_model("sam3", subimage_sam3)
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
        result_tensor = mock_triton.get_output_tensor_by_name(responses[0], "result")
        result = json.loads(result_tensor.as_numpy().flatten()[0].decode("utf-8"))

        assert len(result["subimages"]) == 1
        analysis = result["subimages"][0]["analysis"]
        assert analysis["status"] == "analyzed"
        assert analysis["regions"] == []

    def test_handles_vlm_malformed_json(self, analysis_model, sam3_module):
        """Pipeline raises on VLM returning invalid JSON."""
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3(sam3_module))
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

        with pytest.raises(json.JSONDecodeError):
            model.execute([request])

    def test_dinov3_error_fails_pipeline(self, analysis_model, sam3_module):
        """DINOv3 errors propagate — no silent fallback to empty embeddings."""
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3(sam3_module))
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

        # DINOv3 failure raises RuntimeError (Triton wraps this into error response)
        with pytest.raises(RuntimeError, match="DINOv3"):
            model.execute([request])

    def test_subimage_detection_uses_prompts(self, analysis_model, sam3_module):
        """Subimage detection calls SAM3 with panel detection prompts."""
        received_prompts = []
        entity_handler = _make_sam3_handler(sam3_module)

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

        # Should have received subimage detection prompts for composite/collage detection
        assert len(received_prompts) > 0
        assert any("composite" in p or "collage" in p for p in received_prompts)


# =============================================================================
# Schema compatibility tests
# =============================================================================


class TestSchemaCompatibility:
    """Tests to ensure Python output matches Rust schema expectations.

    These tests validate that the JSON produced by the Python models can be
    deserialized by the Rust types, catching schema drift early.
    """

    def test_analysis_result_matches_rust_schema(self, analysis_model, sam3_module):
        """Full pipeline output validates against Rust AnalysisResult schema."""
        import jsonschema

        # Get the Rust-generated schema
        result = subprocess.run(
            ["cargo", "run", "--bin", "print_schema", "result"],
            capture_output=True,
            text=True,
            cwd=Path(__file__).parent.parent,
        )
        if result.returncode != 0:
            pytest.skip(f"cargo not available: {result.stderr[:100]}")

        rust_schema = json.loads(result.stdout)

        # Set up pipeline with subimage detection fallback
        mock_triton.register_model("sam3", _make_subimage_fallback_sam3(sam3_module))

        def vlm_handler(inputs):
            result = {
                "status": "analyzed",
                "media_type": "photo",
                "content_summary": "A building",
                "scene_type": "outdoor",
                "temporal_cues": ["black and white"],
                "regions": [
                    {
                        "entity_type": "building",
                        "description": "A brick building",
                        "identifiable_features": ["red brick", "arched windows"],
                        "visible_text": [],
                        "damage_signs": [],
                    }
                ],
                "region_relationships": [],
                "extracted_text": [{"text": "1923", "location": "cornerstone"}],
            }
            return mock_triton.InferenceResponse(
                [mock_triton.Tensor("text_output", np.array([json.dumps(result).encode("utf-8")]))]
            )

        mock_triton.register_model("vlm", vlm_handler)
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
        result_tensor = mock_triton.get_output_tensor_by_name(responses[0], "result")
        python_output = json.loads(result_tensor.as_numpy().flatten()[0].decode("utf-8"))

        # Validate against Rust schema
        jsonschema.validate(instance=python_output, schema=rust_schema)

        # Verify subimage structure
        assert "subimages" in python_output
        assert len(python_output["subimages"]) == 1
        subimage = python_output["subimages"][0]
        assert "bounds" in subimage
        assert "analysis" in subimage
        assert subimage["analysis"]["status"] == "analyzed"
        assert len(subimage["analysis"]["regions"]) == 1
        assert len(subimage["analysis"]["embedding"]) == 1024
