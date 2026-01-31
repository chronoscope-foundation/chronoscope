#!/bin/bash
# Deploy Triton analysis server to Northflank
#
# Prerequisites:
#   - Northflank CLI installed and authenticated
#   - Apple Container CLI available
#   - GHCR login: container registry login ghcr.io -u <github-username>
#
# Usage:
#   ./deploy.sh                                              # Full build and deploy
#   ./deploy.sh --skip-build                                 # Template-only deploy (no container build)
#   NF_PROJECT=chronoscope-us-east ./deploy.sh --skip-build  # Deploy to different project
#   NF_GPU_PLAN=nf-gpu-h100-80-1g VLM_MODEL=Qwen/Qwen3-VL-32B-Instruct ./deploy.sh
#   NF_GPU_PLAN=nf-gpu-l4-24-1g ./deploy.sh                  # Budget L4 for testing

set -euo pipefail

# Parse arguments
SKIP_BUILD=false
for arg in "$@"; do
    case $arg in
        --skip-build)
            SKIP_BUILD=true
            ;;
    esac
done

# Configuration
PROJECT="${NF_PROJECT:-chronoscope}"
REGISTRY="ghcr.io"
GHCR_REPO="${GHCR_REPO:-copumpkin/chronoscope-triton}"
GPU_PLAN="${NF_GPU_PLAN:-nf-gpu-a100-80-1g}"
VLM_MODEL="${VLM_MODEL:-}"  # Default in vlm/config.pbtxt
IMAGE_TAG="${IMAGE_TAG:-latest}"

# Get the directory containing this script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== Chronoscope Analysis Deployment ==="
echo "Project:   $PROJECT"
echo "GPU Plan:  $GPU_PLAN"
echo "VLM Model: ${VLM_MODEL:-<default: Qwen/Qwen3-VL-32B-Instruct>}"
echo "Image Tag: $IMAGE_TAG"
echo ""

# 1. Build and push container (skip with --skip-build for template-only changes)
if [[ "$SKIP_BUILD" == "false" ]]; then
    # Build with Apple Container (amd64 for Northflank's NVIDIA GPUs)
    # Use more memory/CPUs for large pip installs in Triton base images
    echo "Building container for linux/amd64..."
    container build --platform linux/amd64 --memory 8G --cpus 4 --progress plain ${NO_CACHE:+--no-cache} -t "$REGISTRY/$GHCR_REPO:$IMAGE_TAG" "$SCRIPT_DIR/triton"

    # Push to GHCR
    echo ""
    echo "Pushing to GHCR..."
    container image push "$REGISTRY/$GHCR_REPO:$IMAGE_TAG"
else
    echo "Skipping container build (--skip-build)"
fi

# 2. Create/update and run template
echo ""
echo "Deploying to Northflank..."

# Upsert template (update if exists, create if not)
echo "Updating template (or creating if it doesn't exist)..."
northflank update template --templateId triton-analysis --file "$SCRIPT_DIR/northflank/template.json" \
    || northflank create template --file "$SCRIPT_DIR/northflank/template.json"

# Run the template with project-specific arguments
echo "Running template..."
northflank run template --templateId triton-analysis --quiet \
    -i "{\"arguments\":{\"projectId\":\"$PROJECT\"}}"

echo ""
echo "=== Deployment Complete ==="
echo ""
echo "To connect to the service:"
echo "  northflank forward service --projectId $PROJECT --serviceId triton --localPort 8000 --port 8000"
echo ""
echo "Then run analysis:"
echo "  cargo run -p chronoscope-analysis --bin analyze -- <image>"
