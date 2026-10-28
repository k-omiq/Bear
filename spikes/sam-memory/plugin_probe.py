"""Run SAM through the WebGPU plugin EP (the Windows/Linux package) from Python.
usage: plugin_probe.py <plugin.dylib> <encoder> <head> <out_prefix> <input.f32>...
"""
import sys, time, numpy as np, onnxruntime as ort
sys.path.insert(0, sys.argv[0].rsplit("/", 1)[0])
import sam_mem_lib as s
plugin, enc, head, out, *inputs = sys.argv[1:]
ort.register_execution_provider_library("WebGpuPlugin", plugin)
devs = [d for d in ort.get_ep_devices() if d.ep_name != "CPUExecutionProvider"]
print("devices", [(d.ep_name, d.ep_vendor, str(d.device.type), d.device.vendor) for d in devs])
def sess(p):
    so = ort.SessionOptions(); so.intra_op_num_threads = 4
    so.add_session_config_entry("session.disable_cpu_ep_fallback", "1")
    so.add_provider_for_devices(devs, {})
    return ort.InferenceSession(p, so)
t = time.time(); e, h = sess(enc), sess(head); print("load", round(time.time()-t,1), "footprint", [round(x/2**30,2) for x in s.footprint()])
for i, f in enumerate(inputs):
    x = np.fromfile(f, "<f4").reshape(1,3,1024,1024)
    t = time.time(); emb = e.run(None, {"prepared_rgb": x})[0]; l = h.run(None, {"embedding": emb})[0]
    l.astype("<f4").tofile(f"{out}_{i}.f32")
    print("page", i, round(time.time()-t,2), "s footprint", [round(v/2**30,2) for v in s.footprint()])
