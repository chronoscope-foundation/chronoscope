---
name: triton
description: NVIDIA Triton Inference Server. Use when deploying ML models with Triton, writing model configurations (config.pbtxt), creating ensemble models, using BLS (Business Logic Scripting), or querying Triton APIs and metrics.
user-invocable: false
---

# Triton Inference Server Guide

NVIDIA Triton Inference Server is a high-performance inference serving platform supporting multiple ML frameworks (TensorRT, ONNX, PyTorch, TensorFlow, Python backend, etc.).

## Quick Reference

**Default Ports**: HTTP 8000, gRPC 8001, Metrics 8002

**Start Server**:
```bash
docker run --gpus=1 --rm -p8000:8000 -p8001:8001 -p8002:8002 \
  -v /path/to/model_repository:/models \
  nvcr.io/nvidia/tritonserver:25.10-py3 \
  tritonserver --model-repository=/models
```

## Model Repository Structure

```
model_repository/
├── my_model/
│   ├── config.pbtxt          # Model configuration
│   └── 1/                    # Version directory
│       └── model.onnx        # Model file (varies by backend)
└── python_model/
    ├── config.pbtxt
    └── 1/
        └── model.py          # Python backend
```

## Minimal config.pbtxt

```protobuf
name: "my_model"
backend: "onnxruntime"
max_batch_size: 8
input [
  {
    name: "input"
    data_type: TYPE_FP32
    dims: [ 3, 224, 224 ]
  }
]
output [
  {
    name: "output"
    data_type: TYPE_FP32
    dims: [ 1000 ]
  }
]
```

**Common Backends**: `tensorrt`, `onnxruntime`, `pytorch`, `tensorflow`, `python`, `openvino`, `dali`

## HTTP API (KServe V2)

```bash
# Health checks
curl http://localhost:8000/v2/health/ready
curl http://localhost:8000/v2/models/{model}/ready

# Model metadata
curl http://localhost:8000/v2/models/{model}
curl http://localhost:8000/v2/models/{model}/config

# Inference
curl -X POST http://localhost:8000/v2/models/{model}/infer \
  -H "Content-Type: application/json" \
  -d '{"inputs": [{"name": "input", "shape": [1,3,224,224], "datatype": "FP32", "data": [...]}]}'

# Statistics
curl http://localhost:8000/v2/models/{model}/stats
```

## Metrics (Prometheus)

```bash
curl http://localhost:8002/metrics

# Key metrics:
# nv_inference_request_success{model="...",version="1"}
# nv_inference_request_failure{model="...",version="1"}
# nv_inference_request_duration_us{model="...",version="1"}
# nv_inference_queue_duration_us{model="...",version="1"}
```

## Dynamic Batching

```protobuf
dynamic_batching {
  preferred_batch_size: [ 4, 8 ]
  max_queue_delay_microseconds: 1000
}
```

## Instance Groups (GPU/CPU Allocation)

```protobuf
instance_group [
  { count: 2, kind: KIND_GPU, gpus: [ 0, 1 ] }
]
# KIND_GPU, KIND_CPU, KIND_MODEL (for TensorRT)
```

## BLS (Business Logic Scripting)

Python models can call other models via BLS:

```python
import triton_python_backend_utils as pb_utils

class TritonPythonModel:
    async def execute(self, requests):
        responses = []
        for request in requests:
            input_tensor = pb_utils.get_input_tensor_by_name(request, "INPUT")

            # Call another model
            infer_request = pb_utils.InferenceRequest(
                model_name='other_model',
                requested_output_names=['OUTPUT'],
                inputs=[input_tensor]
            )
            infer_response = await infer_request.async_exec()

            output = pb_utils.get_output_tensor_by_name(infer_response, 'OUTPUT')
            responses.append(pb_utils.InferenceResponse([output]))
        return responses
```

## Ensemble Models

Chain models without custom code:

```protobuf
name: "ensemble"
platform: "ensemble"
max_batch_size: 8
input [ { name: "IMAGE", data_type: TYPE_FP32, dims: [3, 224, 224] } ]
output [ { name: "RESULT", data_type: TYPE_FP32, dims: [1000] } ]
ensemble_scheduling {
  step [
    {
      model_name: "preprocess"
      model_version: -1
      input_map { key: "RAW", value: "IMAGE" }
      output_map { key: "PROCESSED", value: "preprocessed" }
    },
    {
      model_name: "classifier"
      model_version: -1
      input_map { key: "INPUT", value: "preprocessed" }
      output_map { key: "OUTPUT", value: "RESULT" }
    }
  ]
}
```

**Ensemble vs BLS**: Use ensembles for simple linear/DAG pipelines. Use BLS when you need loops, conditionals, or complex data-dependent logic.

For complete documentation including all configuration options, data types, decoupled models, response caching, and advanced patterns, see [reference.md](reference.md).
