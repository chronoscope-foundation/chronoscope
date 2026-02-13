"""Pytest configuration and fixtures for Triton model tests."""

import importlib.util
import json
import os
import sys
from pathlib import Path

import pytest

# Install mock before any model imports
import mock_triton

sys.modules["triton_python_backend_utils"] = mock_triton

_models_dir = Path(__file__).parent / "models"


def _load_triton_model(model_name: str, args: dict[str, str] | None = None):
    """Load and initialize a TritonPythonModel from the model repository."""
    model_dir = _models_dir / model_name / "1"
    spec = importlib.util.spec_from_file_location(f"{model_name}_fixture", model_dir / "model.py")
    if spec is None or spec.loader is None:
        raise ImportError(f"Could not load model from {model_dir / 'model.py'}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    model = module.TritonPythonModel()
    model.initialize(args or {"model_config": json.dumps({})})
    return model


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
    return _load_triton_model("sam3")


@pytest.fixture(scope="session")
def dinov3_model():
    """Session-scoped real DINOv3 model instance."""
    return _load_triton_model("dinov3")


def _vlm_skip_handler(inputs: dict[str, mock_triton.Tensor]) -> mock_triton.InferenceResponse:
    """VLM handler that always returns an error (VLM not available locally)."""
    return mock_triton.InferenceResponse(
        error=mock_triton.TritonError("VLM not available in local mode")
    )


@pytest.fixture
def pipeline(sam3_model, dinov3_model, reset_mock_triton):
    """Register real SAM3/DINOv3 + VLM skip and return an initialized BLS orchestrator.

    Uses the actual TritonPythonModel classes from the model repository,
    so tests exercise the same code that runs on the Triton server.

    reset_mock_triton is listed explicitly to guarantee it clears the registry
    before this fixture registers real model handlers.
    """
    mock_triton.register_model_instance("sam3", sam3_model)
    mock_triton.register_model_instance("dinov3", dinov3_model)
    mock_triton.register_model("vlm", _vlm_skip_handler)

    return _load_triton_model(
        "analysis",
        args={
            "model_config": json.dumps({}),
            "model_repository": str(_models_dir / "analysis"),
        },
    )
