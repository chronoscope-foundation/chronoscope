# Triton Inference Server with vLLM backend
FROM nvcr.io/nvidia/tritonserver:26.01-vllm-python-py3

# Install dependencies for SAM3, image processing, and DINOv3
RUN pip install --no-cache-dir \
    pillow \
    numpy \
    pycocotools \
    transformers

# Install SAM3 from GitHub (requires HF_TOKEN for model weights)
RUN pip install --no-cache-dir git+https://github.com/facebookresearch/sam3.git

# Download default font for annotation
RUN apt-get update && apt-get install -y --no-install-recommends \
    fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

# Git SHA for provenance tracking (pass via --build-arg GIT_SHA=$(git rev-parse --short HEAD))
# Placed before COPY so that a new commit busts the cache for model configs,
# working around Apple Container's broken COPY cache invalidation.
ARG GIT_SHA=unknown
ENV ANALYSIS_GIT_SHA=$GIT_SHA

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
