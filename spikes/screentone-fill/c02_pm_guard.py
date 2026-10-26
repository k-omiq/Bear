"""Candidate: remnant guard, then plain PatchMatch."""
import guard
import pm


def fill(gray, hole, avoid=None):
    used = guard.grow(gray, hole, avoid)
    return pm.fill(gray, used, avoid), used
