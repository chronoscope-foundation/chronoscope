"""Export the full SAM 3 image model to ONNX: all heads, nothing excised.

One image encode feeds either decoder, so the prompt style is a runtime choice,
not a re-export: concept/grounding (text or box-exemplar -> every instance) and
interactive (box/point -> the one object) both ride on the same 1008px backbone
pass. We keep the language encoder rather than baking it out, so text prompts
stay available.

These four graphs are the whole published image architecture: the Perception
Encoder's two halves (image and text), the DETR grounding detector, and the SAM
2-lineage tracker's prompt-to-mask head. Only the tracker's memory bank is left
out, since it is the video path and this is image segmentation.

The reusable wrappers (the RoPE-real conversion, the language and grounding
decoders) come from samexporter; this script adds the two things samexporter's
grounding-only export omits: an image encoder that also emits the SAM 2-lineage
interactive features, and the single-object interactive decoder. The pieces are
traced from the same `Sam3Processor`/`Sam3Image` the reference runs, so the
graphs match it.

Usage: export-sam3.py <output-dir>
"""

import json
import sys
from pathlib import Path

import torch
from sam3.model.sam3_image import Sam3Image
from sam3.model.sam3_image_processor import Sam3Processor
from sam3.model_builder import build_sam3_image_model
from samexporter.export_sam3 import (
    SAM3Decoder,
    SAM3ImageEncoder,
    SAM3LanguageEncoder,
    get_replace_freqs_cis,
)

OPSET = 18


class MergedImageEncoder(SAM3ImageEncoder):
    """The grounding encoder plus the interactive (SAM 2-lineage) features.

    One backbone pass yields both: the six grounding tensors the base class
    returns, and the three `sam2_backbone_out`-derived tensors the interactive
    decoder reads. `sam2_backbone_out` is a distinct feature pyramid, so
    projecting it here cannot perturb the grounding tensors; the six stay
    byte-identical to the grounding-only export. Mirrors `Sam3Processor.set_image`
    (the conv_s0/s1 adapters) and `Sam3Image.predict_inst`'s feature prep.
    """

    def forward(self, image: torch.Tensor) -> tuple[torch.Tensor, ...]:
        model = self._processor.model
        predictor = model.inst_interactive_predictor
        tracker = predictor.model
        mask_decoder = tracker.sam_mask_decoder

        image = self._transform(image).unsqueeze(0)
        backbone_out = model.backbone._forward_image_no_act_ckpt(image)

        assert len(backbone_out["vision_pos_enc"]) == 3
        assert len(backbone_out["backbone_fpn"]) == 3
        grounding = (*backbone_out["vision_pos_enc"], *backbone_out["backbone_fpn"])

        sam2 = backbone_out["sam2_backbone_out"]
        assert sam2 is not None, "enable_inst_interactivity=True is required"
        sam2["backbone_fpn"][0] = mask_decoder.conv_s0(sam2["backbone_fpn"][0])
        sam2["backbone_fpn"][1] = mask_decoder.conv_s1(sam2["backbone_fpn"][1])
        _, vision_feats, _, _ = tracker._prepare_backbone_features(sam2)
        vision_feats[-1] = vision_feats[-1] + tracker.no_mem_embed
        feats = [
            feat.permute(1, 2, 0).view(1, -1, *feat_size)
            for feat, feat_size in zip(
                vision_feats[::-1], predictor._bb_feat_sizes[::-1]
            )
        ][::-1]
        image_embed, high_res_feat_0, high_res_feat_1 = feats[-1], feats[0], feats[1]

        return (*grounding, image_embed, high_res_feat_0, high_res_feat_1)


class SAM3InteractiveDecoder(torch.nn.Module):
    """Single-object mask from a box/point prompt: the SAM prompt encoder and
    mask decoder, no presence head and no concept filter.

    `multimask_output` and `repeat_image` are Python control flow inside the mask
    decoder, so they are baked: `multimask_output=True` yields three ambiguity
    candidates and the caller keeps `argmax(iou_predictions)`. `masks=None` bakes
    the no-mask dense embedding, correct for a first click.
    """

    def __init__(self, model: Sam3Image) -> None:
        super().__init__()
        tracker = model.inst_interactive_predictor.model
        self._prompt_encoder = tracker.sam_prompt_encoder
        self._mask_decoder = tracker.sam_mask_decoder

    def forward(
        self,
        image_embed: torch.Tensor,
        high_res_feat_0: torch.Tensor,
        high_res_feat_1: torch.Tensor,
        point_coords: torch.Tensor,
        point_labels: torch.Tensor,
    ) -> tuple[torch.Tensor, torch.Tensor]:
        sparse, dense = self._prompt_encoder(
            points=(point_coords, point_labels), boxes=None, masks=None
        )
        low_res_masks, iou_predictions, _, _ = self._mask_decoder(
            image_embeddings=image_embed,
            image_pe=self._prompt_encoder.get_dense_pe(),
            sparse_prompt_embeddings=sparse,
            dense_prompt_embeddings=dense,
            multimask_output=True,
            repeat_image=False,
            high_res_features=[high_res_feat_0, high_res_feat_1],
        )
        return low_res_masks, iou_predictions


@torch.no_grad()
def export_sam3(output_dir: Path) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)

    def model_path(name: str) -> Path:
        # torch spills tensors past the 2GB protobuf limit into sibling files
        # named after graph nodes, and node numbering restarts per export, so a
        # shared directory silently overwrites weights. One directory per graph.
        directory = output_dir / name
        directory.mkdir(parents=True, exist_ok=True)
        return directory / f"sam3_{name}.onnx"

    def record(name: str, facts: dict) -> None:
        (output_dir / name / f"{name}.json").write_text(json.dumps(facts, indent=2))

    # CPU: the fused addmm_act path runs on `mat1.is_cuda` and has no ONNX
    # symbolic, and pinning keeps the store path independent of a builder GPU.
    device = "cpu"
    model = build_sam3_image_model(device=device, enable_inst_interactivity=True)
    assert model.inst_interactive_predictor is not None, (
        "the interactive predictor was not built; enable_inst_interactivity must be True"
    )
    get_replace_freqs_cis(model)
    processor = Sam3Processor(model, device=device)
    model.to(device)
    # The wrappers hold the model on a plain attribute, so the tracer inlines its
    # weights as constants; it refuses tensors that require grad.
    for parameter in model.parameters():
        parameter.requires_grad_(False)

    resolution = processor.resolution
    dummy_image = torch.zeros(3, resolution, resolution, dtype=torch.uint8).to(device)
    dummy_tokens = torch.zeros(1, 32, dtype=torch.long).to(device)

    # ── Image encoder (grounding + interactive features) ─────────────────────
    print("Exporting image encoder...")
    image_encoder = MergedImageEncoder(processor)
    torch.onnx.utils.export(
        image_encoder,
        args=(dummy_image,),
        f=str(model_path("image_encoder")),
        export_params=True,
        input_names=["image"],
        output_names=[
            "vision_pos_enc_0",
            "vision_pos_enc_1",
            "vision_pos_enc_2",
            "backbone_fpn_0",
            "backbone_fpn_1",
            "backbone_fpn_2",
            "image_embed",
            "high_res_feat_0",
            "high_res_feat_1",
        ],
        opset_version=OPSET,
    )
    record(
        "image_encoder",
        {
            "graph_assertions": [
                {"claim": "resolution", "tensor": "image", "axis": -1}
            ],
            "resolution": resolution,
            "preprocessing": {
                "normalization_baked_into_graph": True,
                "caller_resize": {
                    "mode": "stretch",
                    "preserves_aspect_ratio": False,
                    "channel_order": "rgb",
                    "layout": "chw",
                    "target": [resolution, resolution],
                },
            },
        },
    )

    # ── Language encoder (kept whole; text prompts stay available) ───────────
    print("Exporting language encoder...")
    language_encoder = SAM3LanguageEncoder(processor)
    torch.onnx.utils.export(
        language_encoder,
        args=(dummy_tokens,),
        f=str(model_path("language_encoder")),
        export_params=True,
        input_names=["tokens"],
        output_names=["text_attention_mask", "text_memory", "text_embeds"],
        opset_version=OPSET,
    )
    record("language_encoder", {"graph_assertions": []})

    with torch.no_grad():
        encoded = image_encoder(dummy_image)
        vpe0, vpe1, vpe2, fpn0, fpn1, fpn2 = encoded[:6]
        image_embed, high_res_feat_0, high_res_feat_1 = encoded[6:]
        l_mask, l_feat, l_embed = language_encoder(dummy_tokens)

    # ── Grounding decoder (concept: text or box-exemplar -> instances) ───────
    print("Exporting grounding decoder...")
    torch.onnx.utils.export(
        SAM3Decoder(model, processor),
        args=(
            torch.tensor(resolution).to(device),
            torch.tensor(resolution).to(device),
            vpe0,
            vpe1,
            vpe2,
            fpn0,
            fpn1,
            fpn2,
            l_mask,
            l_feat,
            l_embed,
            torch.zeros(1, 1, 4).to(device),
            torch.ones(1, 1, dtype=torch.long).to(device),
            torch.ones(1, 1, dtype=torch.bool).to(device),
        ),
        f=str(model_path("decoder")),
        export_params=True,
        input_names=[
            "original_height",
            "original_width",
            "vision_pos_enc_0",
            "vision_pos_enc_1",
            "vision_pos_enc_2",
            "backbone_fpn_0",
            "backbone_fpn_1",
            "backbone_fpn_2",
            "language_mask",
            "language_features",
            "language_embeds",
            "box_coords",
            "box_labels",
            "box_masks",
        ],
        output_names=["boxes", "scores", "masks"],
        opset_version=OPSET,
    )
    record(
        "decoder",
        {
            "graph_assertions": [],
            "confidence_threshold": processor.confidence_threshold,
        },
    )

    # ── Interactive decoder (box/point -> the one object) ────────────────────
    print("Exporting interactive decoder...")
    # A box is its two corners with labels 2 (top-left) and 3 (bottom-right), in
    # the [0, resolution] model frame. point_labels must be float32: the prompt
    # encoder concatenates a float padding label.
    pt_coords = torch.tensor(
        [[[252.0, 252.0], [756.0, 756.0]]], dtype=torch.float32
    ).to(device)
    pt_labels = torch.tensor([[2.0, 3.0]], dtype=torch.float32).to(device)
    torch.onnx.utils.export(
        SAM3InteractiveDecoder(model),
        args=(image_embed, high_res_feat_0, high_res_feat_1, pt_coords, pt_labels),
        f=str(model_path("decoder_interactive")),
        export_params=True,
        input_names=[
            "image_embed",
            "high_res_feat_0",
            "high_res_feat_1",
            "point_coords",
            "point_labels",
        ],
        output_names=["low_res_masks", "iou_predictions"],
        dynamic_axes={
            "point_coords": {1: "num_points"},
            "point_labels": {1: "num_points"},
        },
        opset_version=OPSET,
    )
    record(
        "decoder_interactive",
        {
            "graph_assertions": [],
            "task": "interactive_single_object",
            # multimask_output=True is baked; the caller keeps argmax(iou_predictions).
            "num_candidates": 3,
            "low_res_mask_size": model.inst_interactive_predictor.model.low_res_mask_size,
            "mask_threshold": model.inst_interactive_predictor.mask_threshold,
            "prompt": {
                "box_encoding": "two_corners_labels_2_3",
                "coord_frame": "model_pixels",
                "coord_range": [0, resolution],
            },
            "mask_upscale": {
                "where": "caller",
                "mode": "bilinear",
                "align_corners": False,
                "target": "original_hw",
                "then_threshold_gt": 0.0,
            },
        },
    )

    print(f"exported four graphs to {output_dir}")


if __name__ == "__main__":
    export_sam3(Path(sys.argv[1]))
