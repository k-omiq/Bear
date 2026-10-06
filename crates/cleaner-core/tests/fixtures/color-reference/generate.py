# SPDX-License-Identifier: CC0-1.0
# Pillow/libjpeg reference derivative and hand-encoded PNG chunk fixtures.
import pathlib, struct, zlib, hashlib, json
from PIL import Image, ImageCms, features
p=pathlib.Path(__file__).parent
profile=(p/'synthetic-cmyk.icc').read_bytes()
# Normalize creation time to make the checked-in synthetic profile reproducible.
profile=profile[:24]+struct.pack('>6H',2026,10,3,0,0,0)+profile[36:]
(p/'synthetic-cmyk.icc').write_bytes(profile)
for name in ['cmyk-adobe','cmyk-plain','ycck-adobe','ycck-subsampled','ycck-markerless']:
 data=(p/f'{name}.jpg').read_bytes()
 icc=b'ICC_PROFILE\0\x01\x01'+profile
 # ICC profile >64k must be split across JPEG APP2 segments.
 parts=[profile[i:i+65519] for i in range(0,len(profile),65519)]
 chunks=b''.join(b'\xff\xe2'+struct.pack('>H',16+len(part))+b'ICC_PROFILE\0'+bytes([i+1,len(parts)])+part for i,part in enumerate(parts))
 (p/f'{name}-icc.jpg').write_bytes(data[:2]+chunks+data[2:])
 ref=Image.frombytes('CMYK',(16,8),(p/f'{name}.jpg.cmyk').read_bytes())
 out=ImageCms.profileToProfile(ref,ImageCms.ImageCmsProfile(str(p/'synthetic-cmyk.icc')),ImageCms.createProfile('sRGB'),outputMode='RGB',renderingIntent=1)
 (p/f'{name}.srgb').write_bytes(out.tobytes())
selected=Image.frombytes('RGB',(3,1),bytes([255,0,0,128,128,128,40,100,180]))
selected_native=ImageCms.profileToProfile(selected,ImageCms.createProfile('sRGB'),ImageCms.ImageCmsProfile(str(p/'synthetic-cmyk.icc')),outputMode='CMYK',renderingIntent=1)
(p/'selected-srgb.cmyk').write_bytes(selected_native.tobytes())
def chunk(kind,data): return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data))
def png(name,depth,mode,rows,extras=[]):
 data=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',4,len(rows),depth,mode,0,0,0))
 data+=b''.join(chunk(k,v) for k,v in extras)+chunk(b'IDAT',zlib.compress(b''.join(b'\0'+r for r in rows)))+chunk(b'IEND',b'')
 (p/name).write_bytes(data)
png('gray16-key.png',16,0,[struct.pack('>4H',0x1234,0x1235,0,65535)],[(b'tRNS',struct.pack('>H',0x1234))])
png('rgb16-key.png',16,2,[struct.pack('>12H',0x1234,0x2345,0x3456,0x1235,0x2345,0x3456,0,0,0,65535,65535,65535)],[(b'tRNS',struct.pack('>3H',0x1234,0x2345,0x3456))])
png('indexed-alpha.png',2,3,[b'\x1b'],[(b'PLTE',bytes([255,0,0,0,255,0,0,0,255,255,255,255])),(b'tRNS',bytes([0,64,128,255]))])
png('gamma-chrm.png',8,2,[bytes([0,64,128,64,128,192,128,192,255,255,64,0])],[(b'gAMA',struct.pack('>I',100000)),(b'cHRM',struct.pack('>8I',31270,32900,64000,33000,30000,60000,15000,6000))])
png('modern-color.png',8,2,[bytes([0,64,128]*4)],[(b'cICP',bytes([1,13,0,1])),(b'sBIT',bytes([7,7,7])),(b'mDCV',struct.pack('>8H2I',32000,16500,15000,30000,7500,3000,15635,16450,10000000,1)),(b'cLLI',struct.pack('>2I',10000000,4000000))])
png('conflicting-profile.png',8,2,[bytes([0,64,128]*4)],[(b'iCCP',b'CMYK\0\0'+zlib.compress(profile)),(b'sRGB',b'\0')])
png('malformed-profile.png',8,2,[bytes([0,64,128]*4)],[(b'iCCP',b'invalid\0\0'+zlib.compress(b'not an ICC profile'))])
# Pillow composes frames independently of the production decoder.
frames=[Image.new('RGBA',(4,3),color) for color in ['red','blue']]
frames[0].save(p/'animated.gif',save_all=True,append_images=frames[1:],duration=100,loop=0,disposal=2)
frames[0].save(p/'animated.webp',save_all=True,append_images=frames[1:],duration=100,loop=0,lossless=True)
for orientation in range(1,9):
 im=Image.new('RGB',(4,3));im.putdata([(x*60,y*90,(x+y)*30) for y in range(3) for x in range(4)])
 exif=Image.Exif();exif[274]=orientation;im.save(p/f'orientation-{orientation}.jpg',quality=95,subsampling=0,exif=exif)
manifest={'license':'CC0-1.0; synthetic test material, no third-party image content','generators':{'Pillow':Image.__version__,'libjpeg':features.version('jpg'),'lcms':features.version('littlecms2')},'dimensions':{'cmyk-jpeg':[16,8],'png':[4,1]},'cmyk_component_tolerance':2,'srgb_transform_tolerance':3,'sha256':{f.name:hashlib.sha256(f.read_bytes()).hexdigest() for f in sorted(p.iterdir()) if f.is_file() and f.suffix not in ['.py','.c','.md','.json']}}
(p/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
