import os
import shutil
import tempfile
import unittest
from unittest import mock

from spoterm import config


class TestDebugLog(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.orig_debug = config.DEBUG
        self.orig_trace = config._trace
        self.orig_max = getattr(config, "DEBUG_LOG_MAX", 1 << 20)
        config._trace = None
        config.DEBUG = True

    def tearDown(self):
        if config._trace is not None:
            try:
                config._trace.close()
            except OSError:
                pass
        config._trace = self.orig_trace
        config.DEBUG = self.orig_debug
        if hasattr(config, "DEBUG_LOG_MAX"):
            config.DEBUG_LOG_MAX = self.orig_max
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_a_few_lines_written(self):
        with mock.patch("spoterm.config.config_dir", return_value=self.tmp):
            config.debug("line 1")
            config.debug("line 2")
            log = os.path.join(self.tmp, "spoterm.log")
            self.assertTrue(os.path.exists(log))
            with open(log, "r", encoding="utf-8") as f:
                content = f.read()
            self.assertIn("line 1", content)
            self.assertIn("line 2", content)

    def test_cap_and_rotation(self):
        self.assertTrue(hasattr(config, "DEBUG_LOG_MAX"), "DEBUG_LOG_MAX constant missing in config")
        config.DEBUG_LOG_MAX = 2048  # 2 KiB
        with mock.patch("spoterm.config.config_dir", return_value=self.tmp):
            # Write 10 KiB of messages (approx 100 lines of 100 bytes)
            payload = "x" * 90
            for i in range(100):
                config.debug(f"{i:03d} {payload}")

            log = os.path.join(self.tmp, "spoterm.log")
            log_1 = os.path.join(self.tmp, "spoterm.log.1")
            self.assertTrue(os.path.exists(log), "spoterm.log must exist")
            self.assertTrue(os.path.exists(log_1), "spoterm.log.1 must exist after exceeding cap")

            log_size = os.path.getsize(log)
            # stays at or below 2 KiB plus one line (~120 bytes)
            self.assertLessEqual(log_size, 2048 + 200)

            # Total content survived: log has the newest lines, log_1 has the preceding lines
            with open(log, "r", encoding="utf-8") as f:
                cur_content = f.read()
            with open(log_1, "r", encoding="utf-8") as f:
                prev_content = f.read()
            self.assertIn("099", cur_content, "last line must be in current log")
            # The line just before the current file starts must be in log_1
            first_in_cur = cur_content.strip().splitlines()[0]
            cur_first_num = int(first_in_cur.split()[1])
            prev_last_num = f"{cur_first_num - 1:03d}"
            self.assertIn(prev_last_num, prev_content, "preceding line must be at end of rotated log")

    def test_existing_large_log_rotated_at_first_use(self):
        log = os.path.join(self.tmp, "spoterm.log")
        log_1 = os.path.join(self.tmp, "spoterm.log.1")
        # Create an existing 5 MiB log
        with open(log, "wb") as f:
            f.write(b"A" * (5 * 1024 * 1024))

        with mock.patch("spoterm.config.config_dir", return_value=self.tmp):
            config.debug("fresh after large log")
            self.assertTrue(os.path.exists(log_1), "spoterm.log.1 must exist")
            self.assertEqual(os.path.getsize(log_1), 5 * 1024 * 1024)
            self.assertLess(os.path.getsize(log), 1024, "new log must be small")
            with open(log, "r", encoding="utf-8") as f:
                self.assertIn("fresh after large log", f.read())

    def test_rotation_raises_oserror(self):
        log = os.path.join(self.tmp, "spoterm.log")
        with open(log, "wb") as f:
            f.write(b"existing log")
        config.DEBUG_LOG_MAX = 10

        with mock.patch("spoterm.config.config_dir", return_value=self.tmp):
            with mock.patch("os.replace", side_effect=OSError("permission denied")):
                # Must not raise
                config.debug("should trigger rotation and fail gracefully")
                self.assertFalse(config.DEBUG, "DEBUG should become False when rotation fails")

    def test_nothing_written_when_debug_false(self):
        config.DEBUG = False
        with mock.patch("spoterm.config.config_dir", return_value=self.tmp):
            config.debug("invisible message")
            log = os.path.join(self.tmp, "spoterm.log")
            self.assertFalse(os.path.exists(log), "spoterm.log should not be created when DEBUG=False")
