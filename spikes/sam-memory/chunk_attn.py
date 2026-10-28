"""Rewrite SAM-TS-L global attention so each head group runs alone.
Same ops, same order inside each head, so the math per element is unchanged.
usage: chunk_attn.py <in.onnx> <out.onnx> <chunks>
"""
import sys
import numpy as np
import onnx
from onnx import helper, numpy_helper

src, dst, chunks = sys.argv[1], sys.argv[2], int(sys.argv[3])
HEADS, TOK, SIDE = 16, 4096, 64
assert HEADS % chunks == 0
per = HEADS // chunks

m = onnx.load(src)
g = m.graph
by_out = {o: n for n in g.node for o in n.output}
blocks = sorted({n.name.split("/")[2] for n in g.node if n.name.startswith("/image_encoder/blocks.")},
                key=lambda s: int(s.split(".")[1]))
global_blocks = [b for b in blocks
                 if not any(n.op_type == "Pad" and n.name.startswith(f"/image_encoder/{b}/") for n in g.node)]
print("global blocks", global_blocks)


def const(name, arr):
    g.initializer.append(numpy_helper.from_array(np.asarray(arr, dtype=np.int64), name))
    return name


const("chunk_axis", [0])
for i in range(chunks):
    const(f"chunk_start_{i}", [i * per])
    const(f"chunk_end_{i}", [(i + 1) * per])
const("chunk_shape5", [per, SIDE, SIDE, SIDE, SIDE])
const("chunk_shape3", [per, TOK, TOK])

drop = set()
new_nodes_at = {}
for b in global_blocks:
    p = f"/image_encoder/{b}/attn/"
    names = ["MatMul", "Reshape_3", "Add_2", "Add_3", "Reshape_4", "Softmax", "MatMul_1"]
    nodes = {n.name[len(p):]: n for n in g.node if n.name.startswith(p) and n.name[len(p):] in names}
    assert len(nodes) == 7, (b, nodes.keys())
    q, kt = nodes["MatMul"].input
    v = nodes["MatMul_1"].input[1]
    relh = nodes["Add_2"].input[1]
    relw = nodes["Add_3"].input[1]
    out = nodes["MatMul_1"].output[0]
    assert nodes["Add_2"].input[0] == nodes["Reshape_3"].output[0]
    assert nodes["Add_3"].input[0] == nodes["Add_2"].output[0]
    made = []
    parts = {}
    for t, label in ((q, "q"), (kt, "kt"), (v, "v"), (relh, "rh"), (relw, "rw")):
        outs = [f"{p}chunk/{label}_{i}" for i in range(chunks)]
        for i, o in enumerate(outs):
            made.append(helper.make_node("Slice", [t, f"chunk_start_{i}", f"chunk_end_{i}", "chunk_axis"], [o],
                                         name=f"{p}chunk/slice_{label}_{i}"))
        parts[label] = outs
    results = []
    for i in range(chunks):
        c = f"{p}chunk/{i}/"
        made += [
            helper.make_node("MatMul", [parts["q"][i], parts["kt"][i]], [c + "a"], name=c + "MatMul"),
            helper.make_node("Reshape", [c + "a", "chunk_shape5"], [c + "a5"], name=c + "Reshape5"),
            helper.make_node("Add", [c + "a5", parts["rh"][i]], [c + "b5"], name=c + "AddH"),
            helper.make_node("Add", [c + "b5", parts["rw"][i]], [c + "c5"], name=c + "AddW"),
            helper.make_node("Reshape", [c + "c5", "chunk_shape3"], [c + "c"], name=c + "Reshape3"),
            helper.make_node("Softmax", [c + "c"], [c + "s"], name=c + "Softmax", axis=-1),
            helper.make_node("MatMul", [c + "s", parts["v"][i]], [c + "o"], name=c + "MatMul_1"),
        ]
        results.append(c + "o")
    made.append(helper.make_node("Concat", results, [out], name=f"{p}chunk/concat", axis=0))
    for n in nodes.values():
        drop.add(id(n))
    new_nodes_at[id(nodes["MatMul_1"])] = made

rebuilt = []
for n in g.node:
    if id(n) in new_nodes_at:
        rebuilt += new_nodes_at[id(n)]
    elif id(n) not in drop:
        rebuilt.append(n)

# Prune nodes nothing reads any more (the old shape plumbing).
outputs = {o.name for o in g.output}
while True:
    used = outputs | {i for n in rebuilt for i in n.input}
    keep = [n for n in rebuilt if any(o in used for o in n.output)]
    if len(keep) == len(rebuilt):
        break
    rebuilt = keep
del g.node[:]
g.node.extend(rebuilt)
used = {i for n in g.node for i in n.input}
inits = [t for t in g.initializer if t.name in used]
del g.initializer[:]
g.initializer.extend(inits)
onnx.checker.check_model(m)
onnx.save(m, dst)
print("nodes", len(g.node))
