"""DINOv3 ViT-L embedding model with mask-pooled region features.

Accepts a single image and a set of masks. Returns one 1024-dim L2-normalized
embedding per mask: CLS token for full-image queries (null mask), area-weighted
average of patch features for region queries (RLE mask).

One forward pass per image — the patch feature grid is shared across all masks.

The mask-pooling approach follows "Region-Based Representations Revisited"
(Shlapentokh-Rothman et al., CVPR 2024, arxiv.org/abs/2402.02352) which pools
DINOv2 patch features within SAM masks for region retrieval, outperforming
crop-based approaches (0.45 vs 0.27 mAP on COCO). They upsample features to
full resolution before pooling; we instead downsample the mask to the patch
grid (with CLS fallback for tiny regions that collapse to zero weight).
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


def _select_device() -> str:
    """Select best available device: CUDA > MPS > CPU.

    NOTE: This helper is intentionally duplicated across model files. Triton's
    Python backend loads each model in isolation, so there's no clean way to
    share code between models without complicating the deployment structure.
    """
    import torch

    if torch.cuda.is_available():
        return "cuda"
    if hasattr(torch.backends, "mps") and torch.backends.mps.is_available():
        return "mps"
    return "cpu"


def _decompress_rle(counts_str: str) -> list[int]:
    """Decompress COCO compressed RLE string to integer counts.

    NOTE: This helper is intentionally duplicated across model files. Triton's
    Python backend loads each model in isolation, so there's no clean way to
    share code between models without complicating the deployment structure.
    """
    counts: list[int] = []
    x = 0
    shift = 0
    for c in counts_str:
        val = ord(c) - 48
        x |= (val & 0x1F) << shift
        if val & 0x20:
            shift += 5
        else:
            counts.append(x)
            x = 0
            shift = 0
    return counts


def _decode_rle(counts: list[int], height: int, width: int) -> np.ndarray:
    """Decode RLE counts to binary mask.

    NOTE: This helper is intentionally duplicated across model files. Triton's
    Python backend loads each model in isolation, so there's no clean way to
    share code between models without complicating the deployment structure.
    """
    flat = np.zeros(height * width, dtype=np.uint8)
    pos = 0
    is_fg = False
    for run_length in counts:
        if is_fg:
            flat[pos : pos + run_length] = 1
        pos += run_length
        is_fg = not is_fg
    return flat.reshape((height, width), order="F")


class TritonPythonModel:
    """DINOv3 embedding model with mask-pooled region features."""

    def initialize(self, args: dict[str, str]) -> None:
        """Load DINOv3 model and processor on best available device."""
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

        device = _select_device()
        self.device = torch.device(device)
        self.model = self.model.to(self.device)

        self.patch_size = self.model.config.patch_size
        # Align image_size to patch grid: 512 / 14 = 36.57 -> 36 patches -> 504 effective pixels.
        # The processor resizes to image_size, and the model truncates to patch-aligned dimensions.
        self.grid_size = self.image_size // self.patch_size
        if self.image_size % self.patch_size != 0:
            aligned = self.grid_size * self.patch_size
            next_aligned = aligned + self.patch_size
            pb_utils.Logger.log_warn(
                f"image_size={self.image_size} not divisible by "
                f"patch_size={self.patch_size}. "
                f"Effective: {aligned}x{aligned} "
                f"({self.grid_size}x{self.grid_size} patches). "
                f"Consider image_size={aligned} or {next_aligned}."
            )
        self.num_register_tokens = getattr(self.model.config, "num_register_tokens", 4)

        n_params = sum(p.numel() for p in self.model.parameters())
        pb_utils.Logger.log_info(
            f"DINOv3 loaded: {n_params / 1e6:.0f}M params on {self.device} "
            f"(image_size={self.image_size}, patch={self.patch_size}, "
            f"grid={self.grid_size}x{self.grid_size})"
        )

    def execute(self, requests: list[Any]) -> list[Any]:
        """Process embedding requests.

        Input:
            image: single base64-encoded image string
            masks: JSON array of mask specs. Each element is either:
                - null: use CLS token (full-image embedding)
                - {"counts": str, "height": int, "width": int}:
                  pool patch features within compressed-RLE mask
        Output:
            embeddings: JSON array of 1024-dim L2-normalized float arrays
        """
        torch = self.torch
        responses = []

        for request in requests:
            image_tensor = pb_utils.get_input_tensor_by_name(request, "image")
            masks_tensor = pb_utils.get_input_tensor_by_name(request, "masks")

            image_b64 = get_string_from_tensor(image_tensor)
            masks_json = get_string_from_tensor(masks_tensor)
            mask_specs: list[dict[str, Any] | None] = json.loads(masks_json)

            # Decode image
            image_bytes = base64.b64decode(image_b64)
            image = Image.open(io.BytesIO(image_bytes)).convert("RGB")

            # Single forward pass
            inputs = self.processor(
                images=[image],
                return_tensors="pt",
                size={"height": self.image_size, "width": self.image_size},
            )
            inputs = {k: v.to(self.device) for k, v in inputs.items()}

            with torch.no_grad():
                outputs = self.model(**inputs)

            # Sequence layout: [CLS, reg1..regN, patch1, patch2, ...]
            hidden = outputs.last_hidden_state[0]
            cls_token = hidden[0]
            patch_start = 1 + self.num_register_tokens
            patch_tokens = hidden[patch_start:]

            n_patches = patch_tokens.shape[0]
            expected = self.grid_size * self.grid_size
            if n_patches != expected:
                raise RuntimeError(
                    f"Expected {expected} patch tokens "
                    f"({self.grid_size}x{self.grid_size}), got {n_patches}"
                )

            patch_grid = patch_tokens.reshape(self.grid_size, self.grid_size, -1)

            # Compute per-mask embeddings
            embeddings = []
            for mask_spec in mask_specs:
                if mask_spec is None:
                    emb = cls_token
                else:
                    emb = self._pool_within_mask(patch_grid, mask_spec, cls_token)

                emb = torch.nn.functional.normalize(emb.unsqueeze(0), p=2, dim=-1).squeeze(0)
                embeddings.append(emb.cpu().tolist())

            result_json = json.dumps(embeddings)
            output_tensor = pb_utils.Tensor("embeddings", np.array([result_json.encode("utf-8")]))
            responses.append(pb_utils.InferenceResponse([output_tensor]))

        return responses

    def _pool_within_mask(
        self,
        patch_grid: Any,  # torch.Tensor [grid_h, grid_w, dim]
        mask_spec: dict[str, Any],
        cls_fallback: Any,  # torch.Tensor [dim]
    ) -> Any:  # torch.Tensor [dim]
        """Weighted average of patch features within a mask.

        Downsamples the mask to the patch grid using area averaging (BOX filter),
        giving each patch a weight proportional to its mask coverage. Falls back
        to CLS if the mask covers zero patches (e.g., a tiny region).
        """
        counts = _decompress_rle(mask_spec["counts"])
        mask = _decode_rle(counts, mask_spec["height"], mask_spec["width"])

        # Downsample mask to patch grid via area averaging
        mask_pil = Image.fromarray(mask * 255, mode="L")
        mask_grid = (
            np.array(
                mask_pil.resize((self.grid_size, self.grid_size), Image.Resampling.BOX),
                dtype=np.float32,
            )
            / 255.0
        )

        weights = self.torch.from_numpy(mask_grid).to(self.device)
        total_weight = weights.sum()

        if total_weight < 1e-6:
            return cls_fallback

        weighted = patch_grid * weights.unsqueeze(-1)
        return weighted.sum(dim=(0, 1)) / total_weight

    def finalize(self) -> None:
        """Clean up."""
        pass
