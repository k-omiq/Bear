# SPDX-License-Identifier: CC0-1.0
# Independent TIFF encodings and native/rendered reference samples.
from pathlib import Path
import hashlib, json
import numpy as np
import tifffile
from PIL import Image
p=Path(__file__).parent
for bits in [8,16]:
    maximum=(1<<bits)-1
    straight=np.array([[[maximum,maximum//2,0,maximum//2],[maximum//3,maximum//4,maximum,maximum],[maximum,0,maximum,0]]],dtype=f'uint{bits}')
    for associated in [False,True]:
        pixels=straight.copy()
        if associated:
            pixels[:,:,:3]=np.rint(pixels[:,:,:3].astype(np.float64)*pixels[:,:,3:4]/maximum).astype(pixels.dtype)
        name=f"{'associated' if associated else 'straight'}-rgba{bits}"
        tifffile.imwrite(p/f'{name}.tif',pixels,photometric='rgb',extrasamples=['ASSOCALPHA' if associated else 'UNASSALPHA'],metadata=None)
        decoded=tifffile.imread(p/f'{name}.tif')
        (p/f'{name}.native').write_bytes(decoded.astype('>u2').tobytes() if bits==16 else decoded.tobytes())
        if bits==8:
            # Pillow independently honors ExtraSamples association on RGBA TIFF.
            (p/f'{name}.rgba').write_bytes(Image.open(p/f'{name}.tif').convert('RGBA').tobytes())
    gray=np.array([[[maximum//4,maximum//2],[maximum,maximum],[0,0]]],dtype=f'uint{bits}')
    tifffile.imwrite(p/f'associated-graya{bits}.tif',gray,photometric='minisblack',extrasamples=['ASSOCALPHA'],metadata=None)
    (p/f'associated-graya{bits}.native').write_bytes(gray.astype('>u2').tobytes() if bits==16 else gray.tobytes())
# Valid legacy transfer/color declarations without ICC require an explicit
# interpretation; treating these native RGB codes as untagged sRGB is unsafe.
pixels=np.array([[[32,64,128],[128,192,255]]],dtype='uint8')
for tag,typ,count,value in [(301,'H',768,tuple(range(256))*3),(318,'2I',2,(3127,10000,3290,10000)),(319,'2I',6,(64,100,33,100,30,100,60,100,15,100,6,100))]:
    tifffile.imwrite(p/f'legacy-color-{tag}.tif',pixels,photometric='rgb',extratags=[(tag,typ,count,value,False)],metadata=None)
manifest={'license':'CC0-1.0 synthetic fixtures','tifffile':tifffile.__version__,'numpy':np.__version__,'sha256':{f.name:hashlib.sha256(f.read_bytes()).hexdigest() for f in sorted(p.iterdir()) if f.is_file() and f.name.startswith(('associated-','straight-','legacy-color-'))}}
(p/'tiff-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
