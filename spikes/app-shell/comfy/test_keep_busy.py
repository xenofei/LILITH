"""Unit tests of keep_busy.py's pure parts. Run: python -I -m unittest discover -s spikes/app-shell/comfy -p "test_*.py" """

import os
import random
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import keep_busy  # noqa: E402


class ToSubmit(unittest.TestCase):
    def test_fills_up_to_depth(self):
        self.assertEqual(keep_busy.to_submit(0, 2), 2)
        self.assertEqual(keep_busy.to_submit(1, 2), 1)

    def test_never_negative(self):
        self.assertEqual(keep_busy.to_submit(2, 2), 0)
        self.assertEqual(keep_busy.to_submit(5, 2), 0)


class Finished(unittest.TestCase):
    def test_success_and_error_are_finished(self):
        self.assertTrue(keep_busy.finished({"status": {"status_str": "success"}}))
        self.assertTrue(keep_busy.finished({"status": {"status_str": "error"}}))
        self.assertTrue(keep_busy.finished({"status": {"completed": True}}))

    def test_missing_or_running_is_not(self):
        self.assertFalse(keep_busy.finished(None))
        self.assertFalse(keep_busy.finished({}))
        self.assertFalse(keep_busy.finished({"status": {"status_str": "running"}}))


class Prepare(unittest.TestCase):
    def test_fresh_seeds_and_preview_only(self):
        base = {
            "1": {"class_type": "KSampler", "inputs": {"seed": 0, "steps": 20}},
            "2": {"class_type": "SaveImage", "inputs": {"filename_prefix": "x", "images": ["1", 0]}},
        }
        p = keep_busy.prepare(base, random.Random(1))
        self.assertNotEqual(p["1"]["inputs"]["seed"], 0)
        self.assertEqual(p["2"]["class_type"], "PreviewImage")
        self.assertNotIn("filename_prefix", p["2"]["inputs"])
        self.assertEqual(base["2"]["class_type"], "SaveImage")  # the base is untouched


if __name__ == "__main__":
    unittest.main()
