"""Candidate S1: remnant guard, screen-only sources, onion-peel phase-locked copy.

1. Grow the hole over leftover lettering (guard).
2. Find the screen's short repeat offsets; with none (no screen), use plain PatchMatch.
3. Sources are known pixels that look like the screen (confidence map), never another detected
   region, lettering, a light or a line.
4. Tone: blur of the sources only, carried across the hole by a membrane (Laplace) fill. The
   PatchMatch tone (T4) picked up lights and black areas in s1-2; S2 compares tone fields properly.
5. Fill from the edge inward through repeat offsets: far ones copy page pixels directly, short ones
   chain through pixels already filled where nothing else reaches. Each pixel takes the source
   closest in tone, preferring page pixels and the offset its neighbours used, and never one more
   than 8 tone levels off.
   Any pixel no offset reaches falls back to plain PatchMatch.
"""
import cv2
import numpy as np

import guard
import pm
import screen


def fill(gray, hole, avoid=None):
    return fill_with(gray, hole, avoid, lambda tone, known, used: screen.membrane(tone, known))


def fill_with(gray, hole, avoid, tone_field, dots=None):
    """The S1 pipeline with the tone field (step 4) passed in, so S2 can swap it, and an optional
    dot step (S3) run after the copy: `dots(gray, used, out, tone_f, src, offs) -> out`."""
    avoid = np.zeros_like(hole) if avoid is None else avoid
    used = guard.grow(gray, hole, avoid)
    raw = gray.astype(np.float64)
    known = ~cv2.dilate(used.astype(np.uint8), np.ones((5, 5), np.uint8)).astype(bool) & ~avoid
    hp = screen.detail(raw, known)
    offs, score = screen.repeat_offsets(hp, known)
    if len(offs) == 0:
        return pm.fill(gray, used, avoid), used
    src, _ = screen.sources(gray, used, avoid, offs)
    tone, w = screen.masked_blur(raw, src, 2.0)
    tone_f = tone_field(tone, w >= 0.25, used)
    far = screen.far_offsets(hp, known, near_best=score[0])
    both = np.unique(np.concatenate([offs, far]), axis=0)
    out, done = screen.onion_copy(gray, used, src, tone_f, both)
    rest = used & ~done
    if rest.any():
        out = np.where(rest, pm.fill(gray, used, avoid), out)
    if dots is not None:
        out = dots(gray, used, out, tone_f, src, both)
    return np.clip(out + 0.5, 0, 255).astype(np.uint8), used
