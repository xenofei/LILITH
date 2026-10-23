"""Unit tests of keep_busy.py. Run: python -I -m unittest discover -s spikes/app-shell/comfy -p "test_*.py" """

import os
import random
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import keep_busy  # noqa: E402

BASE = {
    "1": {"class_type": "KSampler", "inputs": {"seed": 0, "steps": 20}},
    "2": {"class_type": "SaveImage", "inputs": {"filename_prefix": "x", "images": ["1", 0]}},
}


class FakeComfy:
    """A ComfyUI that runs one prompt at a time: the oldest queued prompt finishes after
    `polls_per_prompt` /history polls. Records how many were outstanding at every submit."""

    def __init__(self, polls_per_prompt=3, fail_at=None, bad_json_at=None):
        self.queue = []
        self.done = {}
        self.polls = 0
        self.submitted = 0
        self.outstanding_at_submit = []
        self.polls_per_prompt = polls_per_prompt
        self.fail_at = fail_at
        self.bad_json_at = bad_json_at

    def call(self, method, path, body=None, timeout=10.0):
        if method == "POST" and path == "/prompt":
            self.submitted += 1
            if self.bad_json_at == self.submitted:
                raise ValueError("Expecting value: an HTML error page")
            assert body["prompt"]["2"]["class_type"] == "PreviewImage"
            pid = f"p{self.submitted:04d}"
            self.outstanding_at_submit.append(len(self.queue))
            self.queue.append(pid)
            return {"prompt_id": pid, "node_errors": {}}
        if method == "GET" and path.startswith("/history/"):
            pid = path.split("/")[-1]
            self.polls += 1
            if self.queue and self.polls % self.polls_per_prompt == 0:
                head = self.queue.pop(0)
                n = int(head[1:])
                self.done[head] = {"status": {"status_str": "error" if n == self.fail_at else "success"}}
            return {pid: self.done[pid]} if pid in self.done else {}
        raise AssertionError(f"unexpected call {method} {path}")


def stop_after(fake, prompts):
    return lambda: len(fake.done) >= prompts


class Run(unittest.TestCase):
    def test_keeps_two_outstanding_and_tops_up_after_each_finish(self):
        fake = FakeComfy()
        log = []
        rc = keep_busy.run(fake, BASE, 2, stop_after(fake, 6), rng=random.Random(1), sleep=lambda s: None, log=log)
        self.assertEqual(rc, 0)
        # Never more than 2 outstanding, and after the first fill every submit tops 1 back up to 2.
        self.assertTrue(all(n < 2 for n in fake.outstanding_at_submit))
        self.assertEqual(fake.outstanding_at_submit[:2], [0, 1])
        self.assertTrue(all(n == 1 for n in fake.outstanding_at_submit[2:]))
        self.assertGreaterEqual(len(fake.done), 6)
        self.assertTrue(all(r.get("status") == "success" for r in log if "finished_epoch_ms" in r))

    def test_stops_submitting_when_told(self):
        fake = FakeComfy()
        rc = keep_busy.run(fake, BASE, 2, lambda: fake.submitted >= 2, sleep=lambda s: None)
        self.assertEqual(rc, 0)
        self.assertEqual(fake.submitted, 2)

    def test_a_failed_prompt_ends_the_loop_with_1(self):
        fake = FakeComfy(fail_at=3)
        self.assertEqual(keep_busy.run(fake, BASE, 2, stop_after(fake, 50), sleep=lambda s: None), 1)

    def test_a_non_json_answer_ends_the_loop_with_2(self):
        fake = FakeComfy(bad_json_at=4)
        self.assertEqual(keep_busy.run(fake, BASE, 2, stop_after(fake, 50), sleep=lambda s: None), 2)


class Helpers(unittest.TestCase):
    def test_to_submit(self):
        self.assertEqual(keep_busy.to_submit(0, 2), 2)
        self.assertEqual(keep_busy.to_submit(1, 2), 1)
        self.assertEqual(keep_busy.to_submit(5, 2), 0)

    def test_finished(self):
        self.assertTrue(keep_busy.finished({"status": {"status_str": "success"}}))
        self.assertTrue(keep_busy.finished({"status": {"completed": True}}))
        self.assertFalse(keep_busy.finished(None))
        self.assertFalse(keep_busy.finished({"status": {"status_str": "running"}}))

    def test_prepare_fresh_seeds_and_preview_only(self):
        p = keep_busy.prepare(BASE, random.Random(1))
        self.assertNotEqual(p["1"]["inputs"]["seed"], 0)
        self.assertEqual(p["2"]["class_type"], "PreviewImage")
        self.assertNotIn("filename_prefix", p["2"]["inputs"])
        self.assertEqual(BASE["2"]["class_type"], "SaveImage")  # the base is untouched

    def test_parent_process_check(self):
        self.assertTrue(keep_busy.process_alive(os.getpid()))
        self.assertTrue(keep_busy.process_alive(0))  # no parent given: never stops for it
        self.assertFalse(keep_busy.process_alive(4_000_000))  # no such process


if __name__ == "__main__":
    unittest.main()
