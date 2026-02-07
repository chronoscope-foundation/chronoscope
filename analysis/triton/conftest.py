"""Pytest configuration and fixtures for Triton model tests."""

import importlib.util
import sys
from pathlib import Path

import pytest

# Install mock before any model imports
import mock_triton

sys.modules["triton_python_backend_utils"] = mock_triton


def _load_model_module(model_dir: Path, module_name: str):
    """Load a model.py from a specific directory as a unique module."""
    model_path = model_dir / "model.py"
    spec = importlib.util.spec_from_file_location(module_name, model_path)
    if spec is None or spec.loader is None:
        raise ImportError(f"Could not load module from {model_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


# Model directory
_models_dir = Path(__file__).parent / "models"


@pytest.fixture(scope="module")
def analysis_model():
    """Load the analysis orchestration model."""
    return _load_model_module(_models_dir / "analysis" / "1", "analysis_model")


@pytest.fixture(scope="module")
def sam3_module():
    """Load the SAM3 segmentation model."""
    return _load_model_module(_models_dir / "sam3" / "1", "sam3_model")


@pytest.fixture(scope="module")
def dinov3_module():
    """Load the DINOv3 embedding model."""
    return _load_model_module(_models_dir / "dinov3" / "1", "dinov3_model")


@pytest.fixture(scope="module")
def baml_converter():
    """Load the BAML converter from the analysis model directory."""
    # Load directly without going through the analysis model to avoid import conflicts
    model_path = _models_dir / "analysis" / "1" / "baml_converter.py"
    spec = importlib.util.spec_from_file_location("baml_converter_test", model_path)
    if spec is None or spec.loader is None:
        raise ImportError(f"Could not load module from {model_path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture(autouse=True)
def reset_mock_triton():
    """Reset mock triton state before and after each test.

    This prevents test pollution when a test fails mid-execution,
    ensuring the mock registry doesn't leak state to subsequent tests.
    """
    mock_triton.clear_models()
    yield
    mock_triton.clear_models()
