"""int8 (dynamic, weights only) and fp16 (io kept fp32) variants of both graphs."""
import os, onnx
from onnxruntime.quantization import quantize_dynamic, QuantType
from onnxconverter_common import float16
os.makedirs("dedup", exist_ok=True)
for g in ("vision", "decoder"):
    m = onnx.load(f"onnx/{g}.onnx")
    seen = {}
    for o in m.opset_import:
        seen[o.domain] = max(seen.get(o.domain, 0), o.version)
    del m.opset_import[:]
    m.opset_import.extend(onnx.helper.make_opsetid(d, v) for d, v in seen.items())
    onnx.save(m, f"dedup/{g}.onnx")
for g in ("vision", "decoder"):
    quantize_dynamic(f"dedup/{g}.onnx", f"variants/{g}-int8.onnx", weight_type=QuantType.QInt8,
                     op_types_to_quantize=["MatMul", "Gemm"], extra_options={"MatMulConstBOnly": True})
    m = onnx.load(f"dedup/{g}.onnx")
    m16 = float16.convert_float_to_float16(m, keep_io_types=True, op_block_list=["Resize", "Range", "Shape"])
    onnx.save(m16, f"variants/{g}-fp16.onnx")
for f in sorted(os.listdir("variants")):
    print(f, os.path.getsize("variants/" + f))
