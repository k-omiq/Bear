"""Candidate S2: c04_s1 with tone field T4 PatchMatch tone."""
import c04_s1
import tone


def fill(gray, hole, avoid=None):
    return c04_s1.fill_with(gray, hole, avoid, lambda tone_map, known, used: tone.patchmatch(tone_map, known, used))
