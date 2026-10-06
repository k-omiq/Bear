"""Export Hayai OCR v2.5 Nova to three ONNX graphs.

vision.onnx   pixel_values f32[1,N,768], grid i64[2] (hp, wp)      -> features f32[1,N,768]
prefill.onnx  vision f32[1,M,3072], cos f32[1,M+1,32], sin f32[1,M+1,32]
                                                                  -> logits f32[1,V], present f32[24,1,2,M+1,64]
step.onnx     token i64[1,1], cos f32[1,1,32], sin f32[1,1,32], past f32[24,1,2,L,64]
                                                                  -> logits f32[1,V], present f32[24,1,2,L+1,64]

The caller pixel-unshuffles the vision features (replicate pad to even, 2x2)
and computes the 2D mRoPE tables, so no graph needs a data-dependent shape.
"""
import sys
import numpy as np
import onnx
import torch
import torch.nn as nn
import torch.nn.functional as F
from onnx import TensorProto, helper, numpy_helper
from transformers import AutoModel

M = "JustANormalTinkerer/hayai-ocr-v2.5-nova"
OUT = sys.argv[1] if len(sys.argv) > 1 else "onnx"
model = AutoModel.from_pretrained(M, trust_remote_code=True).eval().requires_grad_(False)
venc = model.vision_encoder.vision_model if hasattr(model.vision_encoder, "vision_model") else model.vision_encoder
dec = model.decoder


def _rms(self, x):
    return x * torch.rsqrt(x.pow(2).mean(-1, keepdim=True) + self.eps) * self.weight


type(dec.final_norm).forward = _rms  # aten::rms_norm has no ONNX export
BOS = 1


class Vision(nn.Module):
    def __init__(self):
        super().__init__()
        self.emb = venc.embeddings.patch_embedding
        self.encoder = venc.encoder
        self.norm = venc.post_layernorm

    def forward(self, pixel_values, pos):
        x = self.emb(pixel_values) + pos
        x = self.encoder(inputs_embeds=x, attention_mask=None).last_hidden_state
        return self.norm(x)


def rope(x, cos, sin):
    # x [b,s,h,64] interleaved pairs; cos/sin [b,s,32]
    x0, x1 = x[..., 0::2], x[..., 1::2]
    c, s = cos.unsqueeze(2), sin.unsqueeze(2)
    return torch.stack([x0 * c - x1 * s, x0 * s + x1 * c], dim=-1).flatten(-2)


def layer_forward(layer, x, cos, sin, past_k, past_v, mask):
    a = layer.attn
    b, s, _ = x.shape
    h = layer.attn_norm(x)
    q = a.q_norm(a.w_q(h).view(b, s, 8, 64))
    k = a.k_norm(a.w_k(h).view(b, s, 2, 64))
    v = a.w_v(h).view(b, s, 2, 64)
    q, k = rope(q, cos, sin), rope(k, cos, sin)
    k, v = k.transpose(1, 2), v.transpose(1, 2)  # [b,2,s,64]
    if past_k is not None:
        k = torch.cat([past_k, k], dim=2)
        v = torch.cat([past_v, v], dim=2)
    kr, vr = k.repeat_interleave(4, dim=1), v.repeat_interleave(4, dim=1)
    att = torch.matmul(q.transpose(1, 2), kr.transpose(-1, -2)) / 8.0
    if mask is not None:
        att = att + mask
    ctx = torch.matmul(att.softmax(-1), vr).transpose(1, 2).reshape(b, s, 512)
    x = x + layer.attn_res_scale * a.w_o(ctx)
    x = x + layer.ffn_res_scale * layer.ffn(layer.ffn_norm(x))
    return x, k, v


class Decoder(nn.Module):
    """One graph for prefill and step. Prefill: vision M>0, tokens [BOS], empty
    past. Step: vision M=0, the last token, the running past. The caller
    supplies the additive mask [1,1,S,L+S] (block causal on prefill)."""
    def __init__(self):
        super().__init__()
        self.dec = dec

    def forward(self, vision, tokens, cos, sin, mask, past):
        p = self.dec.projector
        vis = p.out_norm(p.mlp(p.norm(vision)))
        x = torch.cat([vis, self.dec.token_embeddings(tokens)], dim=1)
        presents = []
        for i, layer in enumerate(self.dec.layers):
            x, k, v = layer_forward(layer, x, cos, sin, past[2 * i], past[2 * i + 1], mask)
            presents += [k, v]
        logits = self.dec.output_head(self.dec.final_norm(x[:, -1]))
        return logits, torch.stack(presents)


def pos_graph():
    """grid i64[2] -> pos f32[1, hp*wp, 768] by antialiased bilinear resize."""
    emb = venc.embeddings
    side = emb.position_embedding_size
    grid = emb.position_embedding.weight.detach().reshape(side, side, -1).permute(2, 0, 1).unsqueeze(0).numpy()
    nodes = [
        helper.make_node("Constant", [], ["one_768"], value=numpy_helper.from_array(np.array([1, 768], np.int64))),
        helper.make_node("Concat", ["one_768", "grid"], ["sizes"], axis=0),
        helper.make_node("Resize", ["pos_grid", "", "", "sizes"], ["resized"], mode="linear",
                         antialias=1, coordinate_transformation_mode="half_pixel"),
        helper.make_node("Constant", [], ["shape"], value=numpy_helper.from_array(np.array([1, 768, -1], np.int64))),
        helper.make_node("Reshape", ["resized", "shape"], ["flat"]),
        helper.make_node("Transpose", ["flat"], ["pos"], perm=[0, 2, 1]),
    ]
    g = helper.make_graph(nodes, "pos",
                          [helper.make_tensor_value_info("grid", TensorProto.INT64, [2])],
                          [helper.make_tensor_value_info("pos", TensorProto.FLOAT, [1, "N", 768])],
                          [numpy_helper.from_array(grid, "pos_grid")])
    return helper.make_model(g, opset_imports=[helper.make_opsetid("", 18)])


if __name__ == "__main__":
    import os
    os.makedirs(OUT, exist_ok=True)
    with torch.no_grad():
        n = 96
        torch.onnx.export(Vision(), (torch.randn(1, n, 768), torch.randn(1, n, 768)), f"{OUT}/vision_core.onnx",
                          input_names=["pixel_values", "pos"], output_names=["features"],
                          dynamic_axes={"pixel_values": {1: "N"}, "pos": {1: "N"}, "features": {1: "N"}},
                          opset_version=18, dynamo=False)
        m, t, L = 24, 1, 3
        torch.onnx.export(Decoder(), (torch.randn(1, m, 3072), torch.tensor([[1]]), torch.randn(1, m + t, 32),
                                      torch.randn(1, m + t, 32), torch.zeros(1, 1, m + t, L + m + t),
                                      torch.randn(24, 1, 2, L, 64)),
                          f"{OUT}/decoder.onnx", input_names=["vision", "tokens", "cos", "sin", "mask", "past"],
                          output_names=["logits", "present"],
                          dynamic_axes={"vision": {1: "M"}, "tokens": {1: "T"}, "cos": {1: "S"}, "sin": {1: "S"},
                                        "mask": {2: "S", 3: "LS"}, "past": {3: "L"}, "present": {3: "LS"}},
                          opset_version=18, dynamo=False)
    pos = pos_graph()
    core = onnx.load(f"{OUT}/vision_core.onnx", load_external_data=True)
    pos.ir_version = core.ir_version
    pos = onnx.compose.add_prefix(pos, "p_")
    merged = onnx.compose.merge_models(pos, core, io_map=[("p_pos", "pos")])
    onnx.save(merged, f"{OUT}/vision.onnx")
    os.remove(f"{OUT}/vision_core.onnx")
    if os.path.exists(f"{OUT}/vision_core.onnx.data"):
        os.remove(f"{OUT}/vision_core.onnx.data")
    print("done")
