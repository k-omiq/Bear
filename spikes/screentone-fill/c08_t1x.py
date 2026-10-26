"""Candidate S2: c04_s1 with tone field T1 membrane solved exactly (sparse), to check the iterative one in c04_s1."""
import c04_s1
import tone


def fill(gray, hole, avoid=None):
    return c04_s1.fill_with(gray, hole, avoid, lambda tone_map, known, used: tone.membrane(tone_map, known))
