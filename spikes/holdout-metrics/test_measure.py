import csv
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

import numpy as np
from PIL import Image


spec = importlib.util.spec_from_file_location("measure", Path(__file__).with_name("measure.py"))
measure = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measure)


class MeasurementTests(unittest.TestCase):
    def test_applied_mask_and_empty_page(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            labels = root / "labels.json"
            predictions = root / "predictions"
            predictions.mkdir()
            lettering = np.zeros((4, 5), dtype=np.uint8)
            lettering[0, 0:2] = 255
            lettering[2, 0:2] = 255
            protected = np.zeros_like(lettering)
            protected[3, 4] = 255
            predicted = np.zeros_like(lettering)
            predicted[0, 0] = 255
            predicted[1, 4] = 255
            predicted[3, 4] = 255
            Image.fromarray(lettering).save(root / "letters.png")
            Image.fromarray(protected).save(root / "art.png")
            Image.fromarray(predicted).save(predictions / "one.png")
            Image.fromarray(np.zeros_like(lettering)).save(root / "empty.png")
            Image.fromarray(np.zeros_like(lettering)).save(predictions / "two.png")
            labels.write_text(json.dumps({"pages": [
                {"id": "one", "prediction": "one.png", "lettering_mask": "letters.png",
                 "protected_art_mask": "art.png", "tags": ["japanese"],
                 "instances": [{"id": "a", "box_xywh": [0, 0, 2, 1], "tags": ["bubble"]},
                               {"id": "b", "box_xywh": [0, 2, 2, 1], "tags": ["sfx"]}]},
                {"id": "two", "prediction": "two.png", "lettering_mask": "empty.png",
                 "protected_art_mask": "empty.png", "tags": ["korean"], "instances": []}
            ]}))
            with (root / "times.csv").open("w", newline="") as stream:
                writer = csv.writer(stream)
                writer.writerows([["page_id", "seconds"], ["one", "12"]])
            result = measure.measure(labels, predictions, root / "times.csv")
            overall = result["overall"]
            self.assertEqual(overall["instance_recall"], 0.5)
            self.assertEqual(overall["complete_page_recall"], 0.5)
            self.assertEqual(overall["text_pixel_recall"], 0.25)
            self.assertEqual(overall["non_text_exposure_pixels"], 2)
            self.assertEqual(overall["protected_art_damage_pixels"], 1)
            self.assertEqual(overall["false_candidates_per_page"], 1)
            self.assertEqual(overall["correction_time_seconds_total"], 12)
            self.assertEqual(result["slices"]["bubble"]["pages"], 1)
            self.assertEqual(result["slices"]["korean"]["complete_page_recall"], 1)

    def test_fusion_component_labels_and_runtime(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pred = root / "pred"
            pred.mkdir()
            letters = np.array([[255, 0, 0], [255, 0, 0]], dtype=np.uint8)
            labels = np.array([[1, 0, 2], [1, 0, 0]], dtype=np.uint16)
            Image.fromarray(letters).save(root / "letters.png")
            Image.fromarray(np.zeros_like(letters)).save(root / "art.png")
            Image.fromarray(labels).save(pred / "p-labels.png")
            (pred / "p.json").write_text(json.dumps({
                "component_labels": {"file": "p-labels.png"},
                "candidates": [{"kind": "sam_component", "mask_label_id": 1},
                               {"kind": "sam_component", "mask_label_id": 2}],
                "timing_ms": {"page_total": 20}, "peak_process_rss_bytes": 1000}))
            manifest = root / "labels.json"
            manifest.write_text(json.dumps({"pages": [{"id": "p", "lettering_mask": "letters.png",
                "protected_art_mask": "art.png", "instances": [{"id": "a", "box_xywh": [0, 0, 1, 2]}]}]}))
            result = measure.measure(manifest, pred)["overall"]
            self.assertEqual(result["false_candidates"], 1)
            self.assertEqual(result["text_pixel_recall"], 1)
            self.assertEqual(result["non_text_exposure_pixels"], 1)
            self.assertEqual(result["runtime_ms_total"], 20)
            self.assertEqual(result["peak_memory_bytes_max"], 1000)

    def test_detector_only_candidate_has_no_write_pixels(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            labels = np.array([[1, 0], [0, 0]], dtype=np.uint16)
            Image.fromarray(labels).save(root / "labels.png")
            (root / "page.json").write_text(json.dumps({
                "component_labels": {"file": "labels.png"},
                "candidates": [{"kind": "sam_component", "mask_label_id": 1},
                               {"kind": "detector_only", "bbox_xywh": [1, 1, 1, 1]}]}))
            predicted, candidates, _ = measure.prediction(root / "page.json", labels.shape)
            self.assertEqual(int(predicted.sum()), 1)
            self.assertEqual(len(candidates), 2)
            self.assertTrue(candidates[1][1, 1])


if __name__ == "__main__":
    unittest.main()
