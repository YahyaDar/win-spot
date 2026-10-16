"""Unit tests for spoterm.ui."""

import unittest

try:
    import curses  # noqa: F401
except ImportError:
    raise unittest.SkipTest("curses not available (pip install windows-curses)")

from spoterm import ui


class TestUi(unittest.TestCase):
    # ── width ───────────────────────────────────────────────────────────────
    def test_width(self):
        self.assertEqual(ui.width(""), 0)
        self.assertEqual(ui.width("hello"), 5)
        self.assertEqual(ui.width("日本語"), 6)
        self.assertEqual(ui.width("e\u0301"), 1)  # 'e' + combining acute

    # ── fit ─────────────────────────────────────────────────────────────────
    def test_fit_ascii_pad_and_truncate(self):
        self.assertEqual(ui.fit("hello", 10), "hello     ")
        self.assertEqual(ui.fit("hello world", 5), "hell…")

    def test_fit_zero_and_negative_width(self):
        self.assertEqual(ui.fit("hello", 0), "")
        self.assertEqual(ui.fit("hello", -3), "")

    def test_fit_right_align(self):
        self.assertEqual(ui.fit("hello", 10, right=True), "     hello")
        self.assertEqual(ui.fit("hello world", 5, right=True), "hell…")

    def test_fit_width_smaller_than_ellipsis(self):
        # Ellipsis is 3 chars, width is 2: truncates to s[:w] without ellipsis
        self.assertEqual(ui.fit("hello", 2, ell="..."), "he")

    def test_fit_custom_ellipsis(self):
        self.assertEqual(ui.fit("hello world", 6, ell="~"), "hello~")

    def test_fit_cjk_widths(self):
        # '日' and '本' are width 2 each (4 total). With ell '…' (width 1), fits in 5 cols.
        res = ui.fit("日本語", 5)
        self.assertEqual(res, "日本…")
        self.assertEqual(ui.width(res), 5)

        # Truncating with 4 cols: only 1 kanji (2) + '…' (1) fits (3 cols) + 1 pad space = 4 cols
        res2 = ui.fit("日本語", 4)
        self.assertEqual(ui.width(res2), 4)

    def test_fit_combining_marks(self):
        res = ui.fit("e\u0301", 3)
        self.assertEqual(ui.width(res), 3)

    def test_fit_control_characters_removed(self):
        res = ui.fit("a\x1bb\nc\td", 10)
        self.assertEqual(res, "abcd      ")

    def test_fit_non_printable_only(self):
        res = ui.fit("\x1b\x00\n\t", 5)
        self.assertEqual(res, "     ")
        self.assertEqual(len(res), 5)

    def test_fit_display_width_always_equals_requested_width(self):
        samples = [
            "hello",
            "a very long ascii title that will definitely be truncated",
            "日本語テキスト",
            "Mixed 英語 and 日本語 with emoji 😀 and combining e\u0301",
            "\x1b[31mANSI\x1b[0m and \n control \t chars",
            "",
            "   spaces   ",
        ]
        for s in samples:
            for w in range(1, 41):
                res = ui.fit(s, w)
                self.assertEqual(
                    ui.width(res),
                    w,
                    f"fit({s!r}, {w}) returned {res!r} with display width {ui.width(res)} != {w}",
                )

    # ── fmt_time ────────────────────────────────────────────────────────────
    def test_fmt_time(self):
        self.assertEqual(ui.fmt_time(0), "0:00")
        self.assertEqual(ui.fmt_time(999), "0:00")
        self.assertEqual(ui.fmt_time(61_000), "1:01")
        self.assertEqual(ui.fmt_time(3_599_000), "59:59")
        self.assertEqual(ui.fmt_time(3_600_000), "1:00:00")
        self.assertEqual(ui.fmt_time(3_661_000), "1:01:01")
        self.assertEqual(ui.fmt_time(-500), "0:00")


if __name__ == "__main__":
    unittest.main()
