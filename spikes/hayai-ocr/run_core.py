import math
from PIL import Image
import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer
BOS, EOS, PAD = 16001, 16002, 16000
PATCHES = 384
FREQS = 1.0 / (10000.0 ** (np.arange(0, 32, 2, dtype=np.float32) / 32))


def target_size(h, w, patch=16, max_patches=PATCHES, eps=1e-5):
    def scaled(scale, size):
        return int(max(patch, math.ceil(size * scale / patch) * patch))
    lo, hi = eps / 10, 100.0
    while hi - lo >= eps:
        s = (lo + hi) / 2
        if (scaled(s, h) / patch) * (scaled(s, w) / patch) <= max_patches:
            lo = s
        else:
            hi = s
    return scaled(lo, h), scaled(lo, w)


def patchify(img):
    w, h = img.size
    th, tw = target_size(h, w)
    a = np.asarray(img.convert("RGB").resize((tw, th), Image.BILINEAR), dtype=np.float32) / 255.0
    a = (a - 0.5) / 0.5
    hp, wp = th // 16, tw // 16
    p = a.reshape(hp, 16, wp, 16, 3).transpose(0, 2, 1, 3, 4).reshape(hp * wp, 768)
    return p[None], hp, wp


def unshuffle(feat, hp, wp):
    f = feat[0].reshape(hp, wp, 768)
    if hp % 2:
        f = np.concatenate([f, f[-1:]], 0)
    if wp % 2:
        f = np.concatenate([f, f[:, -1:]], 1)
    H, W = f.shape[0] // 2, f.shape[1] // 2
    # pixel_unshuffle: channel index = c*4 + dy*2 + dx
    f = f.reshape(H, 2, W, 2, 768).transpose(0, 2, 4, 1, 3).reshape(H * W, 3072)
    return f[None], H, W


def vis_rope(H, W):
    y = np.repeat(np.arange(H, dtype=np.float32), W)
    x = np.tile(np.arange(W, dtype=np.float32), H)
    f = np.concatenate([np.outer(y, FREQS), np.outer(x, FREQS)], -1)
    return f


def text_rope(t):
    f = np.outer(np.array([t], np.float32), FREQS)
    return np.concatenate([f, f], -1)




class Reader:
    def __init__(self, d, provider, tokenizer):
        p = [provider] if provider == "CPUExecutionProvider" else [provider, "CPUExecutionProvider"]
        self.vision = ort.InferenceSession(f"{d}/vision.onnx", providers=p)
        self.decoder = ort.InferenceSession(f"{d}/decoder.onnx", providers=p)
        self.tok = Tokenizer.from_file(tokenizer)

    def read(self, img, max_new=64):
        pv, hp, wp = patchify(img)
        feat = self.vision.run(None, {"p_grid": np.array([hp, wp], np.int64), "pixel_values": pv})[0]
        v, H, W = unshuffle(feat, hp, wp)
        m = H * W
        f = np.concatenate([vis_rope(H, W), text_rope(0)], 0)[None]
        mask = np.zeros((1, 1, m + 1, m + 1), np.float32)
        mask[0, 0, :m, m:] = -1e9
        past = np.zeros((24, 1, 2, 0, 64), np.float32)
        logits, past = self.decoder.run(None, {"vision": v.astype(np.float32), "tokens": np.array([[BOS]], np.int64),
                                          "cos": np.cos(f).astype(np.float32), "sin": np.sin(f).astype(np.float32),
                                          "mask": mask, "past": past})
        ids, probs = [], []
        empty = np.zeros((1, 0, 3072), np.float32)
        for step in range(1, max_new + 1):
            p = np.exp(logits[0] - logits[0].max()); p /= p.sum()
            t = int(p.argmax()); probs.append(float(p[t]))
            if t in (EOS, PAD) or step == max_new:
                break
            ids.append(t)
            f = text_rope(step)[None]
            logits, past = self.decoder.run(None, {"vision": empty, "tokens": np.array([[t]], np.int64),
                                              "cos": np.cos(f).astype(np.float32), "sin": np.sin(f).astype(np.float32),
                                              "mask": np.zeros((1, 1, 1, past.shape[3] + 1), np.float32), "past": past})
        return self.tok.decode(ids, skip_special_tokens=True), probs


