"""read_job.py job raws reason : Hayai readings (fp32 ONNX through check_onnx.py, margin as in Rust) for held regions."""
import json,sys,glob,os
from PIL import Image
sys.argv, args = sys.argv[:1], sys.argv[1:]
import check_onnx as R
job,raws,want=args
j=json.load(open(job)); pages=sorted(glob.glob(os.path.join(raws,"*")))
items=j["detections"] if want=="det" else [r for r in j["regions_untouched"] if want in r.get("reason","")]
for r in items:
    b=r["bbox"]; im=Image.open(pages[r["source_idx"]]).convert("RGB")
    m=max(4,round(max(b["w"],b["h"])*0.12))
    c=im.crop((max(0,b["x"]-m),max(0,b["y"]-m),min(im.width,b["x"]+b["w"]+m),min(im.height,b["y"]+b["h"]+m)))
    t,p=R.read(c); print(f"p{r['source_idx']+1} {b['x']},{b['y']} {t!r} mean={sum(p)/len(p):.2f} min={min(p):.2f}")
