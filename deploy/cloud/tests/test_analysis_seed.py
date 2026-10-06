"""No-network proof that optional cloud graphs are seeded before advertised."""
from __future__ import annotations

import hashlib
import base64
import io
import json
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from deploy.cloud.common.contract import ANALYSIS_RT, ANALYSIS_SAM, ANALYSIS_VERSION
from deploy.cloud.common.contract import analysis_request_digest
from deploy.cloud.common.api import CloudGateway
from PIL import Image
from deploy.cloud.common.analysis_seed import (
    AnalysisUnavailable, analysis_capabilities, installed_graphs, normalize_analysis_models,
    run_analysis_seed, sam_head_export_sha256, seed_analysis_graphs,
)
from deploy.cloud.beam.analysis_backend import BeamAnalysisProxy, beam_analysis_task
from deploy.cloud.beam.backend import BeamMapStore
from deploy.cloud.common.analysis_runtime import OnDemandAnalysis


class MemoryMap:
    def __init__(self):
        self.data = {}

    def get(self, key):
        return self.data.get(key)

    def set(self, key, value, ttl):
        self.data[key] = value

    def __delitem__(self, key):
        del self.data[key]


class AnalysisSeedCases(unittest.TestCase):
    def test_sam_head_export_uses_only_verified_platform_pins(self):
        self.assertEqual(
            sam_head_export_sha256("darwin"),
            "a2c63ccf54e2e692a281cffd4dcda648f252ae6649dc7d23d0203e5868685281",
        )
        self.assertEqual(
            sam_head_export_sha256("linux"),
            "d9431cf1828bbbf70db26f438f5b5777783729dbf7cac5f20cd2b14057125bd2",
        )
        self.assertEqual(sam_head_export_sha256("win32"), sam_head_export_sha256("darwin"))

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        self.source.mkdir()
        for name in ("bootstrap.py", "export_mask.py", "synthetic_page.png"):
            (self.source / name).write_bytes(b"source")
        self.payloads = {
            ANALYSIS_SAM: (("encoder.onnx", b"encoder"), ("head.onnx", b"head")),
            ANALYSIS_RT: (("detector.onnx", b"detector"),),
        }
        manifest = {
            key: tuple((name, len(data), hashlib.sha256(data).hexdigest()) for name, data in files)
            for key, files in self.payloads.items()
        }
        from deploy.cloud.common import analysis_seed
        self.manifest = patch.dict(analysis_seed.GRAPH_FILES, manifest, clear=True)
        self.manifest.start()
        self.addCleanup(self.manifest.stop)

    def _download(self, *, repo_id, filename, revision, local_dir):
        self.assertEqual((repo_id, filename, revision), (
            "ogkalu/comic-text-and-bubble-detector", "detector.onnx",
            "16e8a622f91fabc6b5b65c96d32d1183f8843546"))
        target = Path(local_dir) / filename
        target.write_bytes(self.payloads[ANALYSIS_RT][0][1])
        return str(target)

    def _export(self, args, *, check, timeout):
        self.assertTrue(check)
        self.assertEqual(timeout, 3600)
        root = Path(args[args.index("--root") + 1]) / "proof"
        root.mkdir(parents=True, exist_ok=True)
        for name, data in self.payloads[ANALYSIS_SAM]:
            (root / name).write_bytes(data)

    def test_selection_is_canonical_and_rejects_unknown_or_duplicate_models(self):
        self.assertEqual(normalize_analysis_models([ANALYSIS_RT, ANALYSIS_SAM]), (ANALYSIS_SAM, ANALYSIS_RT))
        for invalid in ([ANALYSIS_RT, ANALYSIS_RT], ["ctd"], "text_regions_rt@1"):
            with self.assertRaises(ValueError):
                normalize_analysis_models(invalid)

    def test_no_graphs_advertises_no_capability_and_corruption_removes_one(self):
        self.assertEqual(analysis_capabilities(self.root, [ANALYSIS_RT, ANALYSIS_SAM])["capabilities"], [])
        seed_analysis_graphs(self.root, [ANALYSIS_RT], self.source, hf_download=self._download)
        self.assertEqual({item["capability"] for item in analysis_capabilities(self.root, [ANALYSIS_RT, ANALYSIS_SAM])["capabilities"]}, {ANALYSIS_RT})
        (self.root / "analysis/rtdetr-v2-full/detector.onnx").write_bytes(b"Detector")  # same size, wrong SHA
        self.assertEqual(analysis_capabilities(self.root, [ANALYSIS_RT])["capabilities"], [])

    def test_rt_download_keeps_hub_local_dir_off_the_volume(self):
        def mounted_download(**kwargs):
            if Path(kwargs["local_dir"]).is_relative_to(self.root):
                raise shutil.SameFileError("Modal volume mount and backing path are the same file")
            return self._download(**kwargs)

        result = seed_analysis_graphs(
            self.root, [ANALYSIS_RT], self.source, hf_download=mounted_download,
        )
        self.assertEqual(result["status"], "seeded")
        self.assertEqual(len(analysis_capabilities(self.root, [ANALYSIS_RT])["capabilities"]), 1)

    def test_selected_sam_and_rt_use_verified_graphs_and_idempotent_markers(self):
        result = seed_analysis_graphs(self.root, [ANALYSIS_RT, ANALYSIS_SAM], self.source,
                                      hf_download=self._download, run=self._export)
        self.assertEqual(result["status"], "seeded")
        graphs, revisions = installed_graphs(self.root, [ANALYSIS_SAM, ANALYSIS_RT], verify_hash=True)
        self.assertEqual(set(graphs), {ANALYSIS_SAM, ANALYSIS_RT})
        self.assertEqual(len(graphs[ANALYSIS_SAM]), 2)
        self.assertEqual(len(revisions[ANALYSIS_RT]), 40)
        self.assertEqual(seed_analysis_graphs(self.root, [ANALYSIS_SAM, ANALYSIS_RT], self.source,
                                               hf_download=self._download, run=self._export)["status"], "present")
        sessions = []
        runtime = OnDemandAnalysis(str(self.root), [ANALYSIS_SAM, ANALYSIS_RT],
                                   session_factory=lambda path: sessions.append(path))
        worker = runtime._load()
        self.assertEqual(sessions, [], "GPU sessions remain lazy until a tile is submitted")
        self.assertEqual({item["capability"] for item in worker.capabilities()["capabilities"]},
                         {ANALYSIS_SAM, ANALYSIS_RT})

    def test_failed_seed_reports_failure_and_never_publishes_readiness(self):
        store = BeamMapStore(MemoryMap())
        def fail(*args, **kwargs):
            raise RuntimeError("wrong graph digest")
        result = run_analysis_seed(self.root, [ANALYSIS_RT], self.source, store, seed=fail)
        self.assertEqual(result["error_code"], "analysis_seed_failed")
        self.assertEqual(store.get("seed:state")["state"], "failed")
        self.assertEqual(analysis_capabilities(self.root, [ANALYSIS_RT])["capabilities"], [])

    def test_beam_proxy_dispatches_a_real_task_result_only_for_installed_graph(self):
        store = BeamMapStore(MemoryMap())
        def enqueue(handle):
            class Runtime:
                def analyze(self, metadata, tile):
                    return {"request_digest": metadata["request_digest"], "mask_png": b"mask"}
            self.assertEqual(beam_analysis_task(Runtime(), store, handle), {"ok": True})
            return "task-1"
        proxy = BeamAnalysisProxy(str(self.root), (ANALYSIS_RT,), store, enqueue)
        with self.assertRaises(RuntimeError):
            proxy.analyze({"capability": ANALYSIS_RT}, b"tile")
        seed_analysis_graphs(self.root, [ANALYSIS_RT], self.source, hf_download=self._download)
        self.assertEqual(proxy.analyze({"capability": ANALYSIS_RT, "request_digest": "a" * 64}, b"tile"),
                         {"request_digest": "a" * 64, "mask_png": b"mask"})

    def test_gateway_reuses_verified_digest_for_tiles_and_rehashes_replaced_graph(self):
        seed_analysis_graphs(self.root, [ANALYSIS_RT], self.source, hf_download=self._download)
        store = BeamMapStore(MemoryMap())
        dispatched = []
        def enqueue(handle):
            dispatched.append(handle)
            class Runtime:
                def analyze(self, metadata, tile):
                    return {"request_digest": metadata["request_digest"], "mask_png": None}
            beam_analysis_task(Runtime(), store, handle)
            return "task-1"
        proxy = BeamAnalysisProxy(str(self.root), (ANALYSIS_RT,), store, enqueue)
        from deploy.cloud.common import analysis_seed
        with patch.object(analysis_seed, "_digest", wraps=analysis_seed._digest) as digest:
            for _ in range(2):
                self.assertEqual(proxy.analyze({"capability": ANALYSIS_RT,
                                                "request_digest": "a" * 64}, b"tile")["mask_png"], None)
            self.assertEqual(digest.call_count, 1, "repeated tile preflight must reuse the verified graph")
            marker = self.root / "analysis/rtdetr-v2-full/ready.json"
            ready = marker.read_text(encoding="utf-8")
            marker.write_text("{}", encoding="utf-8")
            self.assertEqual(proxy.capabilities()["capabilities"], [], "stale marker must not use cache")
            marker.write_text(ready, encoding="utf-8")
            self.assertEqual(len(proxy.capabilities()["capabilities"]), 1)
            self.assertEqual(digest.call_count, 1)
            graph = self.root / "analysis/rtdetr-v2-full/detector.onnx"
            replacement = graph.with_suffix(".replacement")
            replacement.write_bytes(b"Detector")  # same length, wrong hash and a different inode
            replacement.replace(graph)
            self.assertEqual(proxy.capabilities()["capabilities"], [])
            self.assertEqual(digest.call_count, 2, "replacement must be hashed before it can be advertised")
            with self.assertRaises(AnalysisUnavailable):
                proxy.analyze({"capability": ANALYSIS_RT}, b"tile")
            self.assertEqual(len(dispatched), 2, "invalid graph must not start another GPU task")

    def test_gateway_returns_capability_unavailable_without_starting_gpu(self):
        store = BeamMapStore(MemoryMap())
        started = []
        proxy = BeamAnalysisProxy(str(self.root), (ANALYSIS_RT,), store,
                                  lambda handle: started.append(handle))
        image = io.BytesIO()
        Image.new("RGB", (1, 1), "white").save(image, format="PNG")
        tile = image.getvalue()
        request = {"protocol_version": ANALYSIS_VERSION, "capability": ANALYSIS_RT,
                   "graph_sha256s": [self.manifested_sha(ANALYSIS_RT)],
                   "model_revision": "16e8a622f91fabc6b5b65c96d32d1183f8843546",
                   "tile_id": "tile_01", "tile_rect": {"x": 0, "y": 0, "width": 1, "height": 1},
                   "tile_png_sha256": hashlib.sha256(tile).hexdigest(),
                   "source_page_sha256": "e" * 64, "page_width": 1, "page_height": 1,
                   "tile_core": {"x": 0, "y": 0, "width": 1, "height": 1}}
        request["request_digest"] = analysis_request_digest(request)[1]
        body = json.dumps({"metadata": request, "tile_png_b64": base64.b64encode(tile).decode()}).encode()
        status, _, response = CloudGateway("beam", analysis_worker=proxy, trust_edge_auth=True).handle_http_request(
            "POST", "/mc/analysis/v1/analyze", {"content-type": "application/json"}, body)
        self.assertEqual(status, 503)
        self.assertEqual(json.loads(response)["error_code"], "capability_unavailable")
        self.assertEqual(started, [])

    def manifested_sha(self, capability):
        return hashlib.sha256(self.payloads[capability][0][1]).hexdigest()


if __name__ == "__main__":
    unittest.main()
