"""Synchronous analysis/v1 bridge over a scale-to-zero Beam GPU task queue."""
from __future__ import annotations

import secrets
import time
from typing import Any, Callable

from deploy.cloud.beam.backend import BeamMapStore
from deploy.cloud.common.analysis_seed import AnalysisUnavailable, VerifiedGraphCache, analysis_capabilities

WAIT_SECONDS = 200
POLL_SECONDS = 0.25


def _key(handle: str, suffix: str) -> str:
    return f"analysis:{handle}:{suffix}"


class BeamAnalysisProxy:
    def __init__(self, root: str, selected: tuple[str, ...], store: BeamMapStore,
                 enqueue: Callable[[str], str], *, clock: Callable[[], float] = time.monotonic,
                 sleep: Callable[[float], Any] = time.sleep):
        self.root, self.selected, self.store, self.enqueue = root, selected, store, enqueue
        self.clock, self.sleep = clock, sleep
        self._verified_graphs = VerifiedGraphCache()

    def capabilities(self) -> dict[str, Any]:
        return analysis_capabilities(self.root, self.selected, cache=self._verified_graphs)

    def analyze(self, metadata: dict[str, Any], tile_png: bytes) -> dict[str, Any]:
        if metadata.get("capability") not in {item["capability"] for item in self.capabilities()["capabilities"]}:
            raise AnalysisUnavailable("Selected analysis graph is not installed")
        handle = secrets.token_hex(16)
        input_key = _key(handle, "input")
        tile_key = _key(handle, "tile")
        output_key = _key(handle, "output")
        mask_key = _key(handle, "mask")
        blob = self.store.put_blob(tile_key, tile_png)
        self.store.put(input_key, {"metadata": metadata, "tile": blob})
        try:
            self.enqueue(handle)
            deadline = self.clock() + WAIT_SECONDS
            while self.clock() < deadline:
                result = self.store.get(output_key)
                if result is not None:
                    if result.get("ok") is not True:
                        raise RuntimeError("Cloud analysis GPU task failed")
                    mask_info = result.get("mask")
                    mask = self.store.get_blob(mask_key, mask_info) if mask_info is not None else None
                    if mask_info is not None and mask is None:
                        raise RuntimeError("Cloud analysis mask result is incomplete")
                    value = result.get("result")
                    if not isinstance(value, dict):
                        raise RuntimeError("Cloud analysis result is incomplete")
                    return {**value, "mask_png": mask}
                self.sleep(POLL_SECONDS)
            raise TimeoutError("Cloud analysis GPU task did not finish before the gateway deadline")
        finally:
            self.store.delete_blob(tile_key, blob)
            self.store.delete(input_key)
            result = self.store.get(output_key)
            if result is not None and result.get("mask") is not None:
                self.store.delete_blob(mask_key, result["mask"])
            self.store.delete(output_key)


def beam_analysis_task(runtime: Any, store: BeamMapStore, handle: str) -> dict[str, Any]:
    """GPU handler; save a bounded response in the Map, never a task output URL."""
    input_key = _key(handle, "input")
    tile_key = _key(handle, "tile")
    output_key = _key(handle, "output")
    manifest = store.get(input_key)
    try:
        if not isinstance(manifest, dict) or not isinstance(manifest.get("metadata"), dict):
            raise RuntimeError("Analysis request is no longer stored")
        tile = store.get_blob(tile_key, manifest.get("tile"))
        if tile is None:
            raise RuntimeError("Analysis tile is incomplete")
        result = runtime.analyze(manifest["metadata"], tile)
        mask = result.pop("mask_png")
        mask_info = store.put_blob(_key(handle, "mask"), mask) if mask is not None else None
        store.put(output_key, {"ok": True, "result": result, "mask": mask_info})
        return {"ok": True}
    except Exception:
        store.put(output_key, {"ok": False})
        return {"ok": False}
    finally:
        if isinstance(manifest, dict):
            store.delete_blob(tile_key, manifest.get("tile"))
        store.delete(input_key)
