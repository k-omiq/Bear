"""Pick 2 crop boxes per page: most edge detail, and most mid-grey texture (tone/grain)."""
import sys, glob, os, json
import numpy as np
from PIL import Image
C = 384
out = {}
for f in sorted(glob.glob(sys.argv[1] + "/*.png")):
    g = np.asarray(Image.open(f).convert("L"), dtype=np.float32)
    h, w = g.shape
    gx = np.abs(np.diff(g, axis=1))[:-1]; gy = np.abs(np.diff(g, axis=0))[:, :-1]
    edge = gx + gy
    mid = ((g > 40) & (g < 215)).astype(np.float32)[:-1, :-1]
    cands = []
    for y in range(0, h - C, C // 2):
        for x in range(0, w - C, C // 2):
            e = edge[y:y+C, x:x+C]
            cands.append((x, y, float(e.mean()), float((e * mid[y:y+C, x:x+C]).mean() * mid[y:y+C, x:x+C].mean())))
    a = max(cands, key=lambda c: c[2])
    rest = [c for c in cands if abs(c[0]-a[0]) >= C or abs(c[1]-a[1]) >= C]
    b = max(rest, key=lambda c: c[3])
    out[os.path.basename(f)] = [[a[0], a[1], C, C], [b[0], b[1], C, C]]
json.dump(out, open(sys.argv[2], "w"), indent=0)
print(len(out), "pages")
