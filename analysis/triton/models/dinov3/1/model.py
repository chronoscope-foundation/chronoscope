"""DINOv3 ViT-L embedding model for visual similarity.

Produces 1024-dim CLS embeddings from images, L2-normalized for cosine similarity.
Accepts a batch of base64-encoded images and returns embeddings as JSON.
"""

import base64
import io
import json
import os
from typing import Any

import numpy as np
import triton_python_backend_utils as pb_utils
from PIL import Image


def get_string_from_tensor(tensor: Any) -> str:
    """Extract a UTF-8 string from a Triton tensor.

    Handles both 1D and 2D tensor shapes from HTTP vs BLS calls.

    NOTE: This helper is intentionally duplicated across model files. Triton's
    Python backend loads each model in isolation, so there's no clean way to
    share code between models without complicating the deployment structure.
    """
    result: str = tensor.as_numpy().flatten()[0].decode("utf-8")
    return result


class TritonPythonModel:
    """DINOv3 embedding model."""

    def initialize(self, args: dict[str, str]) -> None:
        """Load DINOv3 model and processor."""
        import torch
        from transformers import AutoImageProcessor, AutoModel

        self.torch = torch
        self.model_config = json.loads(args["model_config"])

        # Read image_size from model config parameters
        params = self.model_config.get("parameters", {})
        self.image_size = int(params.get("image_size", {}).get("string_value", "512"))

        # Read model ID from model.json (authoritative — same pattern as VLM)
        model_json_path = os.path.join(os.path.dirname(__file__), "model.json")
        with open(model_json_path) as f:
            model_name: str = json.load(f)["model"]

        pb_utils.Logger.log_info(
            f"Loading DINOv3 model={model_name} (image_size={self.image_size})..."
        )

        # Use shared HF cache on persistent volume (same as SAM3/VLM)
        cache_dir = os.environ.get("HF_HOME", None)

        self.processor = AutoImageProcessor.from_pretrained(model_name, cache_dir=cache_dir)
        self.model = AutoModel.from_pretrained(model_name, cache_dir=cache_dir)
        self.model.eval()

        if not torch.cuda.is_available():
            raise RuntimeError("DINOv3 requires a GPU but none is available")
        self.device = torch.device("cuda")
        self.model = self.model.to(self.device)

        pb_utils.Logger.log_info(f"DINOv3 loaded on {self.device} (image_size={self.image_size})")

    def execute(self, requests: list[Any]) -> list[Any]:
        """Process embedding requests.

        Input: array of base64-encoded image strings.
        Output: JSON string containing array of 1024-dim float arrays.
        """
        torch = self.torch
        responses = []

        for request in requests:
            images_tensor = pb_utils.get_input_tensor_by_name(request, "images")
            image_strings = images_tensor.as_numpy().flatten()

            # Decode base64 images to PIL
            pil_images = []
            for img_b64 in image_strings:
                img_b64_str = img_b64.decode("utf-8") if isinstance(img_b64, bytes) else img_b64
                img_bytes = base64.b64decode(img_b64_str)
                img = Image.open(io.BytesIO(img_bytes)).convert("RGB")
                pil_images.append(img)

            # Process in chunks to avoid GPU OOM with many region crops.
            # At 512x512 with ViT-L, 8 images is ~1GB activation memory.
            CHUNK_SIZE = 8
            all_embeddings = []

            for chunk_start in range(0, len(pil_images), CHUNK_SIZE):
                chunk = pil_images[chunk_start : chunk_start + CHUNK_SIZE]

                inputs = self.processor(
                    images=chunk,
                    return_tensors="pt",
                    size={"height": self.image_size, "width": self.image_size},
                )
                inputs = {k: v.to(self.device) for k, v in inputs.items()}

                with torch.no_grad():
                    outputs = self.model(**inputs)

                # Extract CLS tokens (first token of last_hidden_state).
                # Sequence layout: [CLS, reg1-4, patch1, patch2, ...] — positions
                # 1-4 are register tokens added by DINOv3, not patch embeddings.
                cls_embeddings = outputs.last_hidden_state[:, 0, :]

                # L2-normalize
                cls_embeddings = torch.nn.functional.normalize(cls_embeddings, p=2, dim=-1)
                all_embeddings.append(cls_embeddings)

            # Concatenate chunks and convert to list of lists
            all_embeddings_tensor = torch.cat(all_embeddings, dim=0)
            embeddings_list = all_embeddings_tensor.cpu().numpy().tolist()

            result_json = json.dumps(embeddings_list)
            output_tensor = pb_utils.Tensor("embeddings", np.array([result_json.encode("utf-8")]))
            responses.append(pb_utils.InferenceResponse([output_tensor]))

        return responses

    def finalize(self) -> None:
        """Clean up."""
        pass
