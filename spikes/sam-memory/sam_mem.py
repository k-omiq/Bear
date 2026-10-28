"""Run SAM-TS-L encoder+head on CPU EP in a fresh process with one option set.
Prints JSON: timings, lifetime peak phys_footprint, and a hash of the mask.
usage: sam_mem.py <variant> <encoder.onnx> <head.onnx> <out.npz> page...
"""
import ctypes, hashlib, json, sys, time
import numpy as np
from PIL import Image, ImageFile
import onnxruntime as ort

ImageFile.LOAD_TRUNCATED_IMAGES = True
SIDE = 1024


class RUsageV4(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [
        (n, ctypes.c_uint64) for n in (
            "user_time", "system_time", "pkg_idle_wkups", "interrupt_wkups", "pageins",
            "wired_size", "resident_size", "phys_footprint", "proc_start_abstime",
            "proc_exit_abstime", "child_user_time", "child_system_time",
            "child_pkg_idle_wkups", "child_interrupt_wkups", "child_pageins",
            "child_elapsed_abstime", "diskio_bytesread", "diskio_byteswritten",
            "cpu_time_qos_default", "cpu_time_qos_maintenance", "cpu_time_qos_background",
            "cpu_time_qos_utility", "cpu_time_qos_legacy", "cpu_time_qos_user_initiated",
            "cpu_time_qos_user_interactive", "billed_system_time", "serviced_system_time",
            "logical_writes", "lifetime_max_phys_footprint", "instructions", "cycles",
            "billed_energy", "serviced_energy", "interval_max_phys_footprint", "runnable_time")]


def footprint():
    lib = ctypes.CDLL("/usr/lib/libproc.dylib")
    info = RUsageV4()
    import os
    if lib.proc_pid_rusage(os.getpid(), 4, ctypes.byref(info)) != 0:
        return None, None
    return info.phys_footprint, info.lifetime_max_phys_footprint


def prepare(path):
    img = Image.open(path).convert("RGB")
    w, h = img.size
    longest = max(w, h)
    rw, rh = max(1, round(w * SIDE / longest)), max(1, round(h * SIDE / longest))
    arr = np.asarray(img.resize((rw, rh), Image.BILINEAR), dtype=np.float32)
    chw = np.full((1, 3, SIDE, SIDE), 128, np.float32)
    chw[0, :, :rh, :rw] = arr.transpose(2, 0, 1)
    return chw


VARIANTS = {
    "base": {},
    "noprepack": {"cfg": {"session.disable_prepacking": "1"}},
    "noarena": {"arena": False},
    "nopattern": {"pattern": False},
    "noarena_nopattern": {"arena": False, "pattern": False},
    "lean": {"arena": False, "pattern": False, "cfg": {"session.disable_prepacking": "1"}},
}


def main():
    variant, enc, head, out, *pages = sys.argv[1:]
    v = VARIANTS.get(variant, {})
    so = ort.SessionOptions()
    so.intra_op_num_threads = 4
    so.enable_cpu_mem_arena = v.get("arena", True)
    so.enable_mem_pattern = v.get("pattern", True)
    for k, val in v.get("cfg", {}).items():
        so.add_session_config_entry(k, val)
    t0 = time.time()
    es = ort.InferenceSession(enc, so, providers=["CPUExecutionProvider"])
    hs = ort.InferenceSession(head, so, providers=["CPUExecutionProvider"])
    load = time.time() - t0
    after_load = footprint()[0]
    times, hashes, logits_all = [], [], []
    for p in pages:
        x = prepare(p)
        t = time.time()
        emb = es.run(None, {"prepared_rgb": x})[0]
        logits = hs.run(None, {"embedding": emb})[0]
        times.append(round(time.time() - t, 2))
        mask = (logits.reshape(SIDE, SIDE) > 0)
        hashes.append(hashlib.sha256(mask.tobytes()).hexdigest()[:12])
        logits_all.append(logits.reshape(SIDE, SIDE).astype(np.float32))
    cur, peak = footprint()
    np.savez_compressed(out, *logits_all)
    print(json.dumps({"variant": variant, "load_s": round(load, 1), "page_s": times,
                      "after_load_gib": round(after_load / 2**30, 2),
                      "end_gib": round(cur / 2**30, 2), "peak_gib": round(peak / 2**30, 2),
                      "mask": hashes}))


main()
