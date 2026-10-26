"""Candidate S3: c04_s1 (T1 membrane, D1 copy) plus D2 coverage correction."""
import c04_s1
import dots
import screen


def fill(gray, hole, avoid=None):
    return c04_s1.fill_with(gray, hole, avoid, lambda tone, known, used: screen.membrane(tone, known), dots.coverage)
