#!/usr/bin/env python3
"""Open the PSDs `cleaner_core::export::psd`'s oracle test wrote with psd-tools.

    PSD_ORACLE_DIR=/tmp/psd cargo test -p cleaner-core --lib export::psd -- --ignored
    python3 scripts/check-psd.py /tmp/psd

psd-tools is an independent reader of the format (`pip install psd-tools`),
so a file it opens with the right mode, depth, layer tree, layer masks and
merged pixels is a file written to the specification rather than to our own
reading of it. It is not Photoshop; docs/12 §C3 stays open until a file has
been opened there.
"""

import sys
from pathlib import Path

import numpy as np
from psd_tools import PSDImage
from psd_tools.constants import ColorMode

W, H = 64, 48
MODES = {
    "l": (ColorMode.GRAYSCALE, 1),
    "la": (ColorMode.GRAYSCALE, 2),
    "rgb": (ColorMode.RGB, 3),
    "rgba": (ColorMode.RGB, 4),
    "cmyk": (ColorMode.CMYK, 4),
}


def expect(cond, what):
    if not cond:
        raise AssertionError(what)


def mode_of(name):
    stem = name.split("-")[0]
    letters = stem.rstrip("0123456789")
    depth = int(stem[len(letters):])
    return MODES[letters], depth


def merged_planes(psd, samples):
    """The merged image as raw planes in file order, at the file's depth."""
    data = psd._record.image_data.get_data(psd._record.header)
    expect(len(data) == samples, f"{len(data)} channels in the merged image, wanted {samples}")
    return [np.frombuffer(d, dtype=np.uint8) for d in data]


def check(path):
    (mode, samples), depth = mode_of(path.stem)
    psd = PSDImage.open(path)
    expect(psd.color_mode == mode, f"{path.name}: mode {psd.color_mode}")
    expect(psd.depth == depth, f"{path.name}: depth {psd.depth}")
    expect(psd.channels == samples, f"{path.name}: {psd.channels} channels")
    expect((psd.width, psd.height) == (W, H), f"{path.name}: {psd.width}×{psd.height}")

    expected = np.fromfile(path.with_name(path.stem.rsplit("-", 1)[0] + ".merged"), dtype=np.uint8)
    planes = merged_planes(psd, samples)
    stored = np.concatenate(planes)
    if mode == ColorMode.CMYK:
        stored = 255 - stored
    expect(np.array_equal(stored, expected), f"{path.name}: merged pixels differ from the composite")

    names = [layer.name for layer in psd.descendants()]
    if path.stem.endswith("-flat"):
        expect(names == ["Background"], f"{path.name}: layers {names}")
        return

    tree = ["Background", "Cleaned", "region-0001", "region-0002"]
    if path.stem.endswith("-lettering"):
        expect(names == tree + ["Lettering"], f"{path.name}: layers {names}")
        lettering = psd[2]
        expect(not lettering.visible, f"{path.name}: Lettering is visible")
        expect(lettering.bbox == (4, 4, 16, 14), f"{path.name}: Lettering bbox {lettering.bbox}")
    else:
        expect(names == tree, f"{path.name}: layers {names}")
    group = psd[1]
    expect(group.is_group(), f"{path.name}: Cleaned is not a group")
    region = group[0]
    expect(region.name == "region-0001", f"{path.name}: first region is {region.name}")
    expect(region.bbox == (4, 4, 16, 14), f"{path.name}: region bbox {region.bbox}")
    expect(region.has_mask(), f"{path.name}: region has no mask")
    mask = region.mask
    expect(mask.bbox == (4, 4, 16, 14), f"{path.name}: mask bbox {mask.bbox}")
    mask_pixels = np.asarray(mask.topil()).astype(np.uint8)
    expected_mask = np.fromfile(path.with_name(path.stem.rsplit("-", 1)[0] + ".mask-a"), dtype=np.uint8).reshape(10, 12)
    if depth == 16:
        # PIL renders a 16-bit mask down to 8; the pattern is what matters.
        mask_pixels = (mask_pixels > 0).astype(np.uint8) * 255
    expect(np.array_equal(mask_pixels, expected_mask), f"{path.name}: mask pixels differ")

    background = psd[0]
    expect(background.bbox == (0, 0, W, H), f"{path.name}: background bbox {background.bbox}")
    expect(not background.is_group(), f"{path.name}: background is a group")


def main(directory):
    files = sorted(Path(directory).glob("*.psd"))
    expect(files, f"no .psd files under {directory}")
    for path in files:
        check(path)
        print(f"ok  {path.name}")
    print(f"{len(files)} files opened by psd-tools with the expected structure")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else ".")
