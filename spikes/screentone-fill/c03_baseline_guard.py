"""Candidate: remnant guard, then the baseline (T4 + phase-locked copy)."""
import guard
import lattice


def fill(gray, hole, avoid=None):
    used = guard.grow(gray, hole, avoid)
    return lattice.fill(gray, used, avoid), used
