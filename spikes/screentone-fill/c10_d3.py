"""Candidate S3: c04_s1's screen model and T1 tone, with D3 re-screened dots in place of the copy."""
import c04_s1
import dots
import screen


def fill(gray, hole, avoid=None):
    return c04_s1.fill_with(gray, hole, avoid, lambda tone, known, used: screen.membrane(tone, known), dots.rescreen)
