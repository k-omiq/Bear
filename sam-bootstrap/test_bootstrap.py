"""Pinned source extraction contracts; no model download or PyTorch required."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch


MODULE = importlib.util.spec_from_file_location("sam_bootstrap", Path(__file__).with_name("bootstrap.py"))
bootstrap = importlib.util.module_from_spec(MODULE)
MODULE.loader.exec_module(bootstrap)


class SourceArchiveTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.archive = self.root / "Hi-SAM.zip"
        self.prefix = f"Hi-SAM-{bootstrap.HI_SAM_REVISION}"

    def archive_with(self, entries: dict[str, bytes]) -> None:
        with zipfile.ZipFile(self.archive, "w") as bundle:
            for name, contents in entries.items():
                bundle.writestr(name, contents)

    def test_root_directory_and_files_extract_and_pin(self) -> None:
        self.archive_with({f"{self.prefix}/": b"", f"{self.prefix}/hi_sam/modeling/build.py": b"pinned\n"})
        pinned_hash = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        with patch.object(bootstrap, "HI_SAM_ARCHIVE_SHA256", pinned_hash):
            source = bootstrap.hi_sam(self.root)
        self.assertEqual((source / "hi_sam/modeling/build.py").read_bytes(), b"pinned\n")
        self.assertEqual((source / ".pinned-revision").read_text().strip(), bootstrap.HI_SAM_REVISION)
        self.assertEqual((source / ".archive-sha256").read_text().strip(), pinned_hash)

    def test_wrong_root_and_traversal_are_rejected(self) -> None:
        for name in ("other/build.py", f"{self.prefix}/../escape.py", "/absolute.py"):
            self.archive_with({name: b"bad"})
            with self.subTest(name=name), self.assertRaisesRegex(RuntimeError, "Unexpected"):
                bootstrap.extract_hi_sam_archive(self.archive, self.root / "source")
        self.assertFalse((self.root / "escape.py").exists())

    def test_archive_hash_mismatch_is_rejected_before_extraction(self) -> None:
        self.archive_with({f"{self.prefix}/hi_sam/modeling/build.py": b"bad"})
        bad_archive = self.archive.read_bytes()
        with patch.object(bootstrap.urllib.request, "urlopen", return_value=io.BytesIO(bad_archive)), self.assertRaisesRegex(RuntimeError, "SHA-256 mismatch"):
            bootstrap.hi_sam(self.root)
        self.assertFalse((self.root / "Hi-SAM").exists())


class Response(io.BytesIO):
    def __init__(self, body: bytes, status: int) -> None:
        super().__init__(body)
        self.status = status


class CheckpointDownloadTest(unittest.TestCase):
    BODY = bytes(range(256)) * 40

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        pins = {"MODEL_BYTES": len(self.BODY), "MODEL_SHA256": hashlib.sha256(self.BODY).hexdigest()}
        for name, value in pins.items():
            patcher = patch.object(bootstrap, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.requests: list[str | None] = []

    def serve(self, *, honour_range: bool, fail_first_after: int | None = None):
        def urlopen(request, timeout):
            ranged = request.get_header("Range")
            self.requests.append(ranged)
            if fail_first_after is not None and len(self.requests) == 1:
                class Broken(Response):
                    def read(inner, size=-1):
                        if inner.tell() >= fail_first_after:
                            raise ConnectionResetError("dropped")
                        return super().read(min(size, fail_first_after - inner.tell()))
                return Broken(self.BODY, 200)
            if ranged and honour_range:
                start = int(ranged.removeprefix("bytes=").removesuffix("-"))
                return Response(self.BODY[start:], 206)
            return Response(self.BODY, 200)
        return patch.object(bootstrap.urllib.request, "urlopen", side_effect=urlopen)

    def test_a_dropped_download_resumes_from_its_partial_file(self) -> None:
        with self.serve(honour_range=True, fail_first_after=4096), patch.object(bootstrap.time, "sleep"):
            target = bootstrap.checkpoint(self.root)
        self.assertEqual(target.read_bytes(), self.BODY)
        self.assertEqual(self.requests, [None, "bytes=4096-"])
        self.assertFalse(target.with_name("model.safetensors.part").exists())

    def test_a_server_that_ignores_the_range_restarts_the_file(self) -> None:
        part = self.root / "checkpoint" / "model.safetensors.part"
        part.parent.mkdir(parents=True)
        part.write_bytes(self.BODY[:100])
        with self.serve(honour_range=False):
            target = bootstrap.checkpoint(self.root)
        self.assertEqual(target.read_bytes(), self.BODY)

    def test_a_verified_checkpoint_is_not_downloaded_again(self) -> None:
        target = self.root / "checkpoint" / "model.safetensors"
        target.parent.mkdir(parents=True)
        target.write_bytes(self.BODY)
        with self.serve(honour_range=True):
            bootstrap.checkpoint(self.root)
        self.assertEqual(self.requests, [])

    def test_wrong_bytes_are_refused_and_not_kept(self) -> None:
        with patch.object(bootstrap, "MODEL_SHA256", "0" * 64), self.serve(honour_range=True), \
                self.assertRaisesRegex(RuntimeError, "SHA-256 mismatch"):
            bootstrap.checkpoint(self.root)
        self.assertFalse((self.root / "checkpoint" / "model.safetensors.part").exists())
        self.assertFalse((self.root / "checkpoint" / "model.safetensors").exists())


try:
    import numpy as np
    import onnx
    import onnxruntime
    from onnx import TensorProto, helper, numpy_helper
except ImportError:
    onnx = None


@unittest.skipIf(onnx is None, "needs the bootstrap environment's onnx and onnxruntime")
class PerHeadAttentionTest(unittest.TestCase):
    """A two-head miniature: two global blocks around a windowed one."""

    HEADS, SIDE, CHANNELS = 2, 4, 16

    def model(self):
        heads, side, channels = self.HEADS, self.SIDE, self.CHANNELS
        tokens = side * side
        rng = np.random.default_rng(7)
        initializers = [
            numpy_helper.from_array(rng.standard_normal((heads, tokens, channels)).astype(np.float32), "q_bias"),
            numpy_helper.from_array(np.asarray([heads, side, side, side, side], np.int64), "shape5"),
            numpy_helper.from_array(np.asarray([heads, tokens, tokens], np.int64), "shape3"),
            numpy_helper.from_array(np.zeros(6, np.int64), "pads"),
        ]
        nodes = [helper.make_node("Add", ["x", "q_bias"], ["q0"], name="/image_encoder/patch_embed/Add")]
        # Two global blocks around a windowed one, the encoder's order.
        for block, query in ((0, "q0"), (2, "q2")):
            attn = f"/image_encoder/blocks.{block}/attn/"
            nodes += [
                helper.make_node("MatMul", [query, "kt"], [attn + "scores"], name=attn + "MatMul"),
                helper.make_node("Reshape", [attn + "scores", "shape5"], [attn + "scores5"], name=attn + "Reshape_3"),
                helper.make_node("Add", [attn + "scores5", "rh"], [attn + "with_h"], name=attn + "Add_2"),
                helper.make_node("Add", [attn + "with_h", "rw"], [attn + "with_w"], name=attn + "Add_3"),
                helper.make_node("Reshape", [attn + "with_w", "shape3"], [attn + "flat"], name=attn + "Reshape_4"),
                helper.make_node("Softmax", [attn + "flat"], [attn + "weights"], name=attn + "Softmax", axis=-1),
                helper.make_node("MatMul", [attn + "weights", "v"], [attn + "attended"], name=attn + "MatMul_1"),
            ]
            if block == 0:
                nodes.append(helper.make_node("Pad", [attn + "attended", "pads"], ["q2"],
                                              name="/image_encoder/blocks.1/Pad"))
        nodes[-1].output[0] = "y"
        inputs = [helper.make_tensor_value_info(name, TensorProto.FLOAT, shape) for name, shape in (
            ("x", [heads, tokens, channels]), ("kt", [heads, channels, tokens]),
            ("v", [heads, tokens, channels]), ("rh", [heads, side, side, side, 1]),
            ("rw", [heads, side, side, 1, side]))]
        output = helper.make_tensor_value_info("y", TensorProto.FLOAT, [heads, tokens, channels])
        graph = helper.make_graph(nodes, "mini", inputs, [output], initializers)
        return helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)], ir_version=8)

    def feeds(self):
        rng = np.random.default_rng(11)
        session_inputs = self.model().graph.input
        return {value.name: rng.standard_normal(
            [dimension.dim_value for dimension in value.type.tensor_type.shape.dim]).astype(np.float32)
            for value in session_inputs}

    @staticmethod
    def run_model(path: Path, feeds):
        session = onnxruntime.InferenceSession(str(path), providers=["CPUExecutionProvider"])
        return session.run(None, feeds)[0]

    def test_rewrite_is_bit_identical_and_saves_the_weights_beside_the_graph(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            proof = Path(temporary)
            onnx.save(self.model(), str(proof / bootstrap.ENCODER))
            feeds = self.feeds()
            before = self.run_model(proof / bootstrap.ENCODER, feeds)
            (proof / (bootstrap.ENCODER + ".data")).write_bytes(b"stale side file from an earlier run")
            with patch.object(bootstrap, "ATTENTION_HEADS", self.HEADS), \
                    patch.object(bootstrap, "ATTENTION_SIDE", self.SIDE):
                bootstrap.per_head_encoder(proof)
            rewritten = onnx.load(str(proof / bootstrap.ENCODER), load_external_data=False)
            operators = [node.op_type for node in rewritten.graph.node]
            self.assertEqual(operators.count("Softmax"), 2 * self.HEADS)
            self.assertEqual(operators.count("Slice"), 2 * 5 * self.HEADS)
            self.assertIn("Pad", operators)
            self.assertTrue(any(entry.key == "location" and entry.value == bootstrap.ENCODER + ".data"
                                for tensor in rewritten.graph.initializer for entry in tensor.external_data))
            self.assertEqual(sorted(path.name for path in proof.iterdir()),
                             [bootstrap.ENCODER, bootstrap.ENCODER + ".data"])
            after = self.run_model(proof / bootstrap.ENCODER, feeds)
            self.assertEqual(before.tobytes(), after.tobytes())

    def test_an_unexpected_attention_layout_is_refused(self) -> None:
        model = self.model()
        for node in model.graph.node:
            if node.name.endswith("/Add_3"):
                node.name += "_renamed"
        with self.assertRaisesRegex(RuntimeError, "Unexpected SAM-TS-L attention layout"):
            bootstrap.split_global_attention(model, self.HEADS, self.SIDE)


if __name__ == "__main__":
    unittest.main()
