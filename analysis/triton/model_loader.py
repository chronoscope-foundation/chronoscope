"""Shared model loading and mock registration for local testing.

Used by both conftest.py (pytest) and run_local.py (corpus batch runner).
"""

import importlib.util
import json
import sys
from pathlib import Path

import mock_triton

# Install mock before any model imports.
sys.modules["triton_python_backend_utils"] = mock_triton

MODELS_DIR = Path(__file__).parent / "models"


def load_triton_model(model_name: str, args: dict[str, str] | None = None):
    """Load and initialize a TritonPythonModel from the model repository."""
    model_py = MODELS_DIR / model_name / "1" / "model.py"
    spec = importlib.util.spec_from_file_location(f"{model_name}_model", model_py)
    if spec is None or spec.loader is None:
        raise ImportError(f"Could not load model from {model_py}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    model = module.TritonPythonModel()
    model.initialize(args or {"model_config": json.dumps({})})
    return model


def vlm_skip_handler(inputs: dict[str, mock_triton.Tensor]) -> mock_triton.InferenceResponse:
    """VLM handler that always returns an error (VLM not available locally)."""
    return mock_triton.InferenceResponse(
        error=mock_triton.TritonError("VLM not available in local mode")
    )


def register_pipeline(sam3_model, dinov3_model, vlm_handler):
    """Register models and return the BLS orchestrator."""
    mock_triton.register_model_instance("sam3", sam3_model)
    mock_triton.register_model_instance("dinov3", dinov3_model)
    mock_triton.register_model("vlm", vlm_handler)

    return load_triton_model(
        "analysis",
        args={
            "model_config": json.dumps({}),
            "model_repository": str(MODELS_DIR / "analysis"),
        },
    )
