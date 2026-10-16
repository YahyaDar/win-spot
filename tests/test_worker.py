"""Unit tests for spoterm.worker."""

import queue
import unittest

from spoterm.worker import Worker


class TestWorker(unittest.TestCase):
    def test_worker_submission_order_and_results(self):
        results = queue.SimpleQueue()
        w = Worker("test-worker", results)

        def cb_done_1(v):
            pass

        def cb_done_2(v):
            pass

        w.submit(lambda: "first", on_done=cb_done_1)
        w.submit(lambda: "second", on_done=cb_done_2)

        cb1, val1 = results.get(timeout=2)
        self.assertIs(cb1, cb_done_1)
        self.assertEqual(val1, "first")

        cb2, val2 = results.get(timeout=2)
        self.assertIs(cb2, cb_done_2)
        self.assertEqual(val2, "second")

    def test_worker_exception_handling(self):
        results = queue.SimpleQueue()
        w = Worker("test-worker-err", results)

        def on_err(e):
            pass

        exc = ValueError("boom")

        def fail():
            raise exc

        w.submit(fail, on_error=on_err)

        cb, err = results.get(timeout=2)
        self.assertIs(cb, on_err)
        self.assertIs(err, exc)

    def test_worker_no_callbacks(self):
        results = queue.SimpleQueue()
        w = Worker("test-worker-nocb", results)

        # Job with no callbacks should not fail or kill thread
        w.submit(lambda: 123)
        cb, val = results.get(timeout=2)
        self.assertIsNone(cb)
        self.assertEqual(val, 123)

    def test_worker_survives_exception_and_runs_next_job(self):
        results = queue.SimpleQueue()
        w = Worker("test-worker-recover", results)

        def cb_done(v):
            pass

        def cb_err(e):
            pass

        # Job 1 raises
        w.submit(lambda: 1 / 0, on_error=cb_err)
        err_cb, err_val = results.get(timeout=2)
        self.assertIs(err_cb, cb_err)
        self.assertIsInstance(err_val, ZeroDivisionError)

        # Job 2 runs normally afterwards
        w.submit(lambda: "alive", on_done=cb_done)
        done_cb, done_val = results.get(timeout=2)
        self.assertIs(done_cb, cb_done)
        self.assertEqual(done_val, "alive")


if __name__ == "__main__":
    unittest.main()
