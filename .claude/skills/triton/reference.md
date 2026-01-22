# Triton Inference Server Complete Reference

## Model Repository

### Directory Layout

```
<model-repository-path>/
  <model-name>/
    [config.pbtxt]                    # Model configuration (optional if auto-generated)
    [<output-labels-file> ...]        # Optional label files
    [configs]/                        # Optional custom configs directory
      [<custom-config-file> ...]
    <version>/                        # Numeric version directory (1, 2, 3, etc.)
      <model-definition-file>
    <version>/
      <model-definition-file>
    ...
```

### Model Files by Backend

| Backend | Default Model Filename |
|---------|----------------------|
| TensorRT | `model.plan` |
| ONNX | `model.onnx` |
| PyTorch | `model.pt` |
| TensorFlow SavedModel | `model.savedmodel/` (directory) |
| TensorFlow GraphDef | `model.graphdef` |
| Python | `model.py` |
| OpenVINO | `model.xml` + `model.bin` |

Override with `default_model_filename` in config.pbtxt:
```protobuf
default_model_filename: "my_custom_model.onnx"
```

### Version Control

- Triton loads all numeric directories as versions (1/, 2/, etc.)
- Use `version_policy` to control which versions are active:

```protobuf
version_policy {
  # Load only the latest version
  latest { num_versions: 1 }
}

version_policy {
  # Load specific versions
  specific { versions: [ 1, 3 ] }
}

version_policy {
  # Load all versions
  all { }
}
```

## Model Configuration (config.pbtxt)

### Required Fields

```protobuf
name: "model_name"              # Must match directory name
backend: "onnxruntime"          # Or platform: "onnxruntime_onnx"
max_batch_size: 8               # 0 for non-batching models

input [
  {
    name: "input_tensor"
    data_type: TYPE_FP32
    dims: [ 3, 224, 224 ]       # Excludes batch dimension
  }
]

output [
  {
    name: "output_tensor"
    data_type: TYPE_FP32
    dims: [ 1000 ]
  }
]
```

### Data Types

| Type | Description |
|------|-------------|
| `TYPE_BOOL` | Boolean |
| `TYPE_UINT8`, `TYPE_UINT16`, `TYPE_UINT32`, `TYPE_UINT64` | Unsigned integers |
| `TYPE_INT8`, `TYPE_INT16`, `TYPE_INT32`, `TYPE_INT64` | Signed integers |
| `TYPE_FP16`, `TYPE_FP32`, `TYPE_FP64` | Floating point |
| `TYPE_STRING` | Variable-length string |
| `TYPE_BF16` | Brain floating point 16 |

### Variable Dimensions

Use `-1` for variable dimensions:
```protobuf
input [
  {
    name: "text"
    data_type: TYPE_INT32
    dims: [ -1 ]                # Variable sequence length
  }
]
```

### Optional Inputs

```protobuf
input [
  {
    name: "optional_input"
    data_type: TYPE_FP32
    dims: [ -1 ]
    optional: true
  }
]
```

### Reshaping

Transform tensor shapes at input/output boundaries:
```protobuf
input [
  {
    name: "input"
    data_type: TYPE_INT32
    dims: [ 1 ]
    reshape: { shape: [ ] }     # Flatten [1] to scalar
  }
]
```

## Instance Groups

Control how many model instances run and where:

```protobuf
instance_group [
  {
    count: 2                    # Number of instances
    kind: KIND_GPU              # KIND_GPU, KIND_CPU, KIND_MODEL
    gpus: [ 0 ]                 # Specific GPUs (for KIND_GPU)
  }
]
```

### Multi-GPU Distribution

```protobuf
instance_group [
  { count: 1, kind: KIND_GPU, gpus: [ 0 ] },
  { count: 1, kind: KIND_GPU, gpus: [ 1 ] }
]
```

### CPU Execution

```protobuf
instance_group [
  { count: 4, kind: KIND_CPU }
]
```

## Batching

### Dynamic Batching

Automatically batches requests for better throughput:

```protobuf
dynamic_batching {
  preferred_batch_size: [ 4, 8, 16 ]    # Preferred sizes to form
  max_queue_delay_microseconds: 1000    # Max wait for batch formation
}
```

### Sequence Batching

For stateful models (RNNs, transformers with KV cache):

```protobuf
sequence_batching {
  max_sequence_idle_microseconds: 5000000
  control_input [
    {
      name: "START"
      control [
        { kind: CONTROL_SEQUENCE_START, fp32_false_true: [ 0, 1 ] }
      ]
    },
    {
      name: "READY"
      control [
        { kind: CONTROL_SEQUENCE_READY, fp32_false_true: [ 0, 1 ] }
      ]
    }
  ]
}
```

## Scheduling and Performance

### Rate Limiter

Limit concurrent executions:
```protobuf
rate_limiter {
  resources [
    { name: "MEMORY", count: 1 }
  ]
}
```

### Response Cache

Cache inference results:
```protobuf
response_cache { enable: true }
```

Requires server flag: `--response-cache-byte-size=<bytes>`

## Decoupled Models

For streaming or multi-response models:

```protobuf
model_transaction_policy {
  decoupled: true
}
```

## Backend Parameters

Pass backend-specific options:

```protobuf
parameters {
  key: "EXECUTION_PROVIDERS"
  value: { string_value: "CUDAExecutionProvider" }
}

parameters {
  key: "intra_op_thread_count"
  value: { string_value: "4" }
}
```

### ONNX Runtime Parameters

| Parameter | Description |
|-----------|-------------|
| `EXECUTION_PROVIDERS` | CUDA, CPU, TensorRT, etc. |
| `intra_op_thread_count` | Threads for parallelism within ops |
| `inter_op_thread_count` | Threads for parallelism between ops |
| `enable_memory_arena_shrinkage` | Memory optimization |

### Python Backend Parameters

| Parameter | Description |
|-----------|-------------|
| `EXECUTION_ENV_PATH` | Path to conda environment |
| `FORCE_CPU_ONLY_INPUT_TENSORS` | Force inputs to CPU |

## Ensemble Models

### Configuration

```protobuf
name: "pipeline"
platform: "ensemble"
max_batch_size: 8

input [
  { name: "RAW_IMAGE", data_type: TYPE_UINT8, dims: [ -1 ] }
]
output [
  { name: "CLASSIFICATION", data_type: TYPE_FP32, dims: [ 1000 ] },
  { name: "DETECTION", data_type: TYPE_FP32, dims: [ -1, 6 ] }
]

ensemble_scheduling {
  step [
    {
      model_name: "preprocess"
      model_version: -1           # Latest version
      input_map { key: "INPUT", value: "RAW_IMAGE" }
      output_map { key: "OUTPUT", value: "processed" }
    },
    {
      model_name: "classifier"
      model_version: -1
      input_map { key: "INPUT", value: "processed" }
      output_map { key: "OUTPUT", value: "CLASSIFICATION" }
    },
    {
      model_name: "detector"
      model_version: -1
      input_map { key: "INPUT", value: "processed" }
      output_map { key: "OUTPUT", value: "DETECTION" }
    }
  ]
}
```

### Flow Control

- `input_map`: Maps ensemble input or previous step output to model input
- `output_map`: Maps model output to tensor name for subsequent steps or ensemble output
- Steps execute when all inputs are available (DAG scheduling)

### Memory Management

```protobuf
ensemble_scheduling {
  max_inflight_requests: 16     # Limit concurrent requests for backpressure
  step [...]
}
```

## Business Logic Scripting (BLS)

### Python Model Structure

```python
import triton_python_backend_utils as pb_utils
import numpy as np

class TritonPythonModel:
    def initialize(self, args):
        """Called once when model loads"""
        self.model_config = json.loads(args['model_config'])

    def execute(self, requests):
        """Process inference requests"""
        responses = []
        for request in requests:
            # Get input tensor
            input_tensor = pb_utils.get_input_tensor_by_name(request, "INPUT")
            input_data = input_tensor.as_numpy()

            # Process
            output_data = self.process(input_data)

            # Create output tensor
            output_tensor = pb_utils.Tensor("OUTPUT", output_data)
            response = pb_utils.InferenceResponse([output_tensor])
            responses.append(response)
        return responses

    def finalize(self):
        """Called when model unloads"""
        pass
```

### Synchronous BLS

```python
def execute(self, requests):
    for request in requests:
        input_tensor = pb_utils.get_input_tensor_by_name(request, "INPUT")

        # Create request to another model
        infer_request = pb_utils.InferenceRequest(
            model_name='other_model',
            requested_output_names=['OUTPUT'],
            inputs=[input_tensor]
        )

        # Blocking call
        infer_response = infer_request.exec()

        if infer_response.has_error():
            raise pb_utils.TritonModelException(infer_response.error().message())

        output = pb_utils.get_output_tensor_by_name(infer_response, 'OUTPUT')
```

### Asynchronous BLS

```python
import asyncio

class TritonPythonModel:
    async def execute(self, requests):
        responses = []
        for request in requests:
            input_tensor = pb_utils.get_input_tensor_by_name(request, "INPUT")

            # Create multiple async requests
            infer_request = pb_utils.InferenceRequest(
                model_name='model_a',
                requested_output_names=['OUTPUT'],
                inputs=[input_tensor]
            )

            # Non-blocking calls
            awaits = [infer_request.async_exec() for _ in range(4)]
            infer_responses = await asyncio.gather(*awaits)

            # Process responses
            for resp in infer_responses:
                if resp.has_error():
                    raise pb_utils.TritonModelException(resp.error().message())
                output = pb_utils.get_output_tensor_by_name(resp, 'OUTPUT')

        return responses
```

### BLS with Decoupled Models

```python
infer_response_iterator = await infer_request.async_exec(decoupled=True)
for infer_response in infer_response_iterator:
    # Process streaming responses
    if len(infer_response.output_tensors()) > 0:
        output = pb_utils.get_output_tensor_by_name(infer_response, 'OUTPUT')
```

### Model Loading API

```python
# Load a model dynamically
pb_utils.load_model('model_name')

# Unload a model
pb_utils.unload_model('model_name')
```

## HTTP/REST API (KServe V2)

### Server Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v2/health/live` | GET | Server liveness check |
| `/v2/health/ready` | GET | Server readiness check |
| `/v2` | GET | Server metadata |
| `/v2/models/${MODEL}` | GET | Model metadata |
| `/v2/models/${MODEL}/ready` | GET | Model readiness check |
| `/v2/models/${MODEL}/config` | GET | Model configuration |
| `/v2/models/${MODEL}/stats` | GET | Model statistics |
| `/v2/models/${MODEL}/infer` | POST | Run inference |
| `/v2/models/${MODEL}/versions/${VERSION}/infer` | POST | Infer on specific version |

### Inference Request Format

```json
{
  "id": "optional-request-id",
  "inputs": [
    {
      "name": "input_tensor",
      "shape": [1, 3, 224, 224],
      "datatype": "FP32",
      "data": [0.1, 0.2, ...]
    }
  ],
  "outputs": [
    {
      "name": "output_tensor"
    }
  ]
}
```

### Inference Response Format

```json
{
  "id": "request-id",
  "model_name": "model",
  "model_version": "1",
  "outputs": [
    {
      "name": "output_tensor",
      "shape": [1, 1000],
      "datatype": "FP32",
      "data": [0.001, 0.002, ...]
    }
  ]
}
```

### Binary Data

Use `parameters.binary_data_output: true` for efficient binary responses.

### Model Control

```bash
# Load model
curl -X POST http://localhost:8000/v2/repository/models/{model}/load

# Unload model
curl -X POST http://localhost:8000/v2/repository/models/{model}/unload

# Get repository index
curl http://localhost:8000/v2/repository/index
```

## gRPC API

Use the Triton client libraries for gRPC:

```python
import tritonclient.grpc as grpcclient

client = grpcclient.InferenceServerClient(url="localhost:8001")

# Check health
client.is_server_ready()
client.is_model_ready("model_name")

# Create inputs
inputs = [grpcclient.InferInput("INPUT", [1, 3, 224, 224], "FP32")]
inputs[0].set_data_from_numpy(input_data)

# Inference
result = client.infer("model_name", inputs)
output = result.as_numpy("OUTPUT")
```

## Metrics

### Prometheus Endpoint

```bash
curl http://localhost:8002/metrics
```

### Key Metrics

| Metric | Description |
|--------|-------------|
| `nv_inference_request_success` | Successful inference count |
| `nv_inference_request_failure` | Failed inference count |
| `nv_inference_count` | Total inferences (includes batched) |
| `nv_inference_exec_count` | Execution count (batches) |
| `nv_inference_request_duration_us` | Total request duration |
| `nv_inference_queue_duration_us` | Time in queue |
| `nv_inference_compute_input_duration_us` | Input processing time |
| `nv_inference_compute_infer_duration_us` | Model execution time |
| `nv_inference_compute_output_duration_us` | Output processing time |
| `nv_gpu_utilization` | GPU utilization % |
| `nv_gpu_memory_used_bytes` | GPU memory usage |

### Statistics API

```bash
curl http://localhost:8000/v2/models/{model}/stats
```

Response includes:
- `inference_count`: Total inferences processed
- `execution_count`: Execution batches
- `inference_stats`: Timing breakdowns (queue, compute_input, compute_infer, compute_output)
- `batch_stats`: Statistics per batch size

## Server Configuration

### Common Flags

```bash
tritonserver \
  --model-repository=/models \
  --model-control-mode=explicit \           # poll, explicit, none
  --strict-model-config=false \             # Auto-generate configs
  --log-verbose=1 \                         # Verbosity level
  --http-port=8000 \
  --grpc-port=8001 \
  --metrics-port=8002 \
  --response-cache-byte-size=1073741824 \   # 1GB response cache
  --cuda-memory-pool-byte-size=0:268435456  # 256MB per GPU
```

### Model Control Modes

| Mode | Description |
|------|-------------|
| `none` | Load all models at startup, no changes |
| `poll` | Periodically scan repository for changes |
| `explicit` | Only load/unload via API |

### Custom Configuration Names

```bash
tritonserver --model-config-name=h100
# Looks for configs/h100.pbtxt instead of config.pbtxt
```

## References

### Official Documentation
- [Triton Inference Server Documentation](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/)
- [Model Configuration](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/model_configuration.html)
- [Model Repository](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/model_repository.html)
- [Ensemble Models](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/ensemble_models.html)
- [Business Logic Scripting](https://docs.nvidia.com/deeplearning/triton-inference-server/user-guide/docs/user_guide/bls.html)

### GitHub Repositories
- [triton-inference-server/server](https://github.com/triton-inference-server/server) - Main server
- [triton-inference-server/client](https://github.com/triton-inference-server/client) - Client libraries
- [triton-inference-server/python_backend](https://github.com/triton-inference-server/python_backend) - Python backend

### Additional Resources
- [NVIDIA Technical Blog: Ensemble Models](https://developer.nvidia.com/blog/serving-ml-model-pipelines-on-nvidia-triton-inference-server-with-ensemble-models/)
- [KServe V2 Protocol](https://github.com/kserve/kserve/blob/master/docs/predict-api/v2/required_api.md)
