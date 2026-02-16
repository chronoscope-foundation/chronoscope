"""Pytest configuration and fixtures for Triton model tests."""

import os

import pytest

import mock_triton
from model_loader import load_triton_model, register_pipeline, vlm_skip_handler


@pytest.fixture(autouse=True)
def reset_mock_triton():
    """Reset mock triton state before and after each test.

    This prevents test pollution when a test fails mid-execution,
    ensuring the mock registry doesn't leak state to subsequent tests.
    """
    mock_triton.clear_models()
    yield
    mock_triton.clear_models()


@pytest.fixture(scope="session")
def sam3_model():
    """Session-scoped real SAM3 model instance."""
    os.environ.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")
    return load_triton_model("sam3")


@pytest.fixture(scope="session")
def dinov3_model():
    """Session-scoped real DINOv3 model instance."""
    return load_triton_model("dinov3")


@pytest.fixture
def pipeline(sam3_model, dinov3_model, reset_mock_triton):
    """Register real SAM3/DINOv3 + VLM skip and return an initialized BLS orchestrator.

    Uses the actual TritonPythonModel classes from the model repository,
    so tests exercise the same code that runs on the Triton server.

    reset_mock_triton is listed explicitly to guarantee it clears the registry
    before this fixture registers real model handlers.
    """
    return register_pipeline(sam3_model, dinov3_model, vlm_skip_handler)
