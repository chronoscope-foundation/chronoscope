"""Mock triton_python_backend_utils for local testing.

Provides enough of the pb_utils API to test model orchestration locally.
"""

from collections.abc import Callable, Iterator
from typing import Any

import numpy as np


class Tensor:
    """Mock Triton tensor."""

    def __init__(self, name: str, data):
        if not isinstance(data, np.ndarray):
            raise TypeError(
                f"Tensor data must be numpy.ndarray, got {type(data).__name__}. "
                "Use np.array([...]) to wrap your data."
            )
        self.name = name
        self._data = data

    def as_numpy(self):
        return self._data


class TritonError(Exception):
    """Mock Triton error."""

    def __init__(self, message: str):
        self._message = message
        super().__init__(message)

    def message(self):
        return self._message


class InferenceFuture:
    """Mock future for async BLS execution."""

    def __init__(self, response: "InferenceResponse"):
        self._response = response

    def get(self) -> "InferenceResponse":
        """Get the result of the async execution."""
        return self._response


class InferenceResponse:
    """Mock inference response."""

    _tensors: dict[str, Tensor]
    _error: TritonError | None

    def __init__(
        self,
        output_tensors: list[Tensor] | None = None,
        error: TritonError | None = None,
    ):
        self._tensors = {t.name: t for t in (output_tensors or [])}
        self._error = error

    def has_error(self) -> bool:
        return self._error is not None

    def error(self) -> TritonError | None:
        return self._error

    def output_tensors(self) -> list[Tensor]:
        return list(self._tensors.values())


# Registry for BLS model handlers
_model_handlers: dict[str, Callable[[dict[str, Any]], InferenceResponse]] = {}


def register_model(name: str, handler: Callable[[dict[str, Any]], InferenceResponse]):
    """Register a handler for BLS calls to a model."""
    _model_handlers[name] = handler


def clear_models():
    """Clear all registered model handlers."""
    _model_handlers.clear()


class Logger:
    """Mock logger that prints to stdout."""

    @staticmethod
    def log_info(msg: str):
        print(f"[INFO] {msg}")

    @staticmethod
    def log_warn(msg: str):
        print(f"[WARN] {msg}")

    @staticmethod
    def log_error(msg: str):
        print(f"[ERROR] {msg}")


class InferenceRequest:
    """Mock inference request with BLS support."""

    model_name: str
    requested_output_names: list[str]
    _inputs: dict[str, Tensor]

    def __init__(
        self,
        model_name: str,
        requested_output_names: list[str],
        inputs: list[Tensor],
        timeout: int = 0,
    ):
        self.model_name = model_name
        self.requested_output_names = requested_output_names
        self._inputs = {t.name: t for t in inputs}
        self._timeout_us = timeout

    def inputs(self) -> list[Tensor]:
        return list(self._inputs.values())

    def async_exec(self) -> InferenceFuture:
        """Execute BLS call asynchronously.

        Returns a Future that can be resolved with .get().
        In mock mode, execution happens immediately.
        """
        handler = _model_handlers.get(self.model_name)
        if handler is None:
            response = InferenceResponse(
                error=TritonError(f"No handler registered for model '{self.model_name}'")
            )
        else:
            try:
                response = handler(self._inputs)
            except Exception as e:
                response = InferenceResponse(error=TritonError(str(e)))
        return InferenceFuture(response)

    def exec(self, decoupled: bool = False) -> InferenceResponse | Iterator[InferenceResponse]:
        """Execute BLS call by looking up registered handler.

        Args:
            decoupled: If True, return an iterator of responses (for streaming).
                      Mock just yields a single response.
        """
        handler = _model_handlers.get(self.model_name)
        if handler is None:
            response = InferenceResponse(
                error=TritonError(f"No handler registered for model '{self.model_name}'")
            )
        else:
            try:
                response = handler(self._inputs)
            except Exception as e:
                response = InferenceResponse(error=TritonError(str(e)))

        if decoupled:
            # Return iterator for decoupled mode
            return iter([response])
        return response


def get_input_tensor_by_name(request: InferenceRequest, name: str) -> Tensor | None:
    """Get input tensor by name from a request."""
    return request._inputs.get(name)


def get_output_tensor_by_name(response: InferenceResponse, name: str) -> Tensor | None:
    """Get output tensor by name from a response."""
    return response._tensors.get(name)
