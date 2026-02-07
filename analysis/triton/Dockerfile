# Triton Inference Server with vLLM backend
FROM nvcr.io/nvidia/tritonserver:25.12-vllm-python-py3

# Install dependencies for SAM3, image processing, and DINOv3
RUN pip install --no-cache-dir \
    pillow \
    numpy \
    opencv-python-headless \
    qwen-vl-utils \
    decord \
    pycocotools \
    transformers

# Install iopath from GitHub (the PyPI tarball has issues)
RUN pip install --no-cache-dir git+https://github.com/facebookresearch/iopath.git

# Install SAM3 from GitHub (requires HF_TOKEN for model weights)
RUN pip install --no-cache-dir git+https://github.com/facebookresearch/sam3.git

# Download default font for annotation
RUN apt-get update && apt-get install -y --no-install-recommends \
    fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

# Copy model repository
COPY models /models

# Environment variables
# HuggingFace token for gated models (required for SAM3 and Qwen)
ENV HF_TOKEN=""
# VLM model selection (default in vlm/config.pbtxt)
ENV VLM_MODEL=""

# Expose HTTP and gRPC ports
EXPOSE 8000 8001 8002

CMD ["tritonserver", "--model-repository=/models", "--http-port=8000"]
