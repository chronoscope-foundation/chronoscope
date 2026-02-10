"""Pytest configuration and fixtures for Triton model tests."""

import sys

import pytest

# Install mock before any model imports
import mock_triton

sys.modules["triton_python_backend_utils"] = mock_triton


@pytest.fixture(autouse=True)
def reset_mock_triton():
    """Reset mock triton state before and after each test.

    This prevents test pollution when a test fails mid-execution,
    ensuring the mock registry doesn't leak state to subsequent tests.
    """
    mock_triton.clear_models()
    yield
    mock_triton.clear_models()
