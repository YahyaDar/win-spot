import time
import unittest

from spoterm import api

try:
    import curses  # noqa: F401
    from spoterm import app
    HAS_CURSES = True
except ImportError:
    HAS_CURSES = False


class TestRetryDelay(unittest.TestCase):
    def test_delays(self):
        # retry_after 0, 3, 5, 30, 300, 301, 86400 give 5, 5, 5, 30, 300, 300, 300
        cases = [
            (0, 5.0),
            (3, 5.0),
            (5, 5.0),
            (30, 30.0),
            (300, 300.0),
            (301, 300.0),
            (86400, 300.0),
        ]
        for ra, expected in cases:
            err = api.ApiError(429, "slow down", retry_after=ra)
            self.assertEqual(api.retry_delay(err), expected)

    def test_tolerates_invalid_values(self):
        # None, "x", -4, float("nan"), float("inf") give 5; document float("inf") capped or floor
        class Dummy:
            pass

        for bad in (None, "x", -4, float("nan")):
            d = Dummy()
            d.retry_after = bad
            self.assertEqual(api.retry_delay(d), 5.0)

        # float("inf") treated as 0 gives floor 5.0
        d_inf = Dummy()
        d_inf.retry_after = float("inf")
        self.assertEqual(api.retry_delay(d_inf), 5.0)

        # an exception without the attribute gives 5
        self.assertEqual(api.retry_delay(ValueError("no retry_after")), 5.0)

    def test_load_more_honors_retry_after(self):
        if not HAS_CURSES:
            raise unittest.SkipTest("curses not available")

        class FakeApi:
            def __init__(self):
                self.calls = 0

            def liked_page(self, off):
                self.calls += 1
                raise api.ApiError(429, "slow down", retry_after=30)

        fake_api = FakeApi()
        fake_settings = type("FakeSettings", (), {"ascii": False})()
        a = app.App(None, fake_api, fake_settings, None)
        a.H, a.W = 30, 100

        tl = a._list("liked")
        tl.next_offset = 0
        a.load_more(tl)

        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline:
            a.drain()
            if not tl.loading:
                break
            time.sleep(0.01)

        self.assertFalse(tl.loading)
        diff = tl.retry_at - time.monotonic()
        self.assertTrue(
            30.0 - 0.5 <= diff <= 30.5,
            f"Expected retry_at to be ~30s in future, got {diff:.2f}s",
        )

        # Call load_more again; since retry_at has not passed, no second API call
        a.load_more(tl)
        self.assertEqual(fake_api.calls, 1)
