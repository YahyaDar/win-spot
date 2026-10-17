import os
import sys
import tempfile
import time
import unittest
from unittest import mock

from spoterm import config, engine


class TestEngineLog(unittest.TestCase):
    def setUp(self):
        self.tmp_dir = tempfile.TemporaryDirectory()
        self.tmp = self.tmp_dir.name
        self.config_dir_patcher = mock.patch.object(config, "config_dir", lambda: self.tmp)
        self.config_dir_patcher.start()
        self.log_path = os.path.join(self.tmp, "engine.log")
        self.log_path_1 = os.path.join(self.tmp, "engine.log.1")

    def tearDown(self):
        self.config_dir_patcher.stop()
        self.tmp_dir.cleanup()

    def test_no_existing_log(self):
        self.assertFalse(os.path.exists(self.log_path))
        f = engine._open_log(self.log_path)
        f.close()
        self.assertTrue(os.path.exists(self.log_path))
        self.assertFalse(os.path.exists(self.log_path_1))
        with open(self.log_path, "r", encoding="utf-8") as lf:
            content = lf.read()
        self.assertTrue(content.startswith("--- engine start "))

    def test_existing_log_rotated(self):
        with open(self.log_path, "w", encoding="utf-8") as f:
            f.write("OLD")
        f = engine._open_log(self.log_path)
        f.close()
        self.assertTrue(os.path.exists(self.log_path_1))
        with open(self.log_path_1, "r", encoding="utf-8") as f1:
            self.assertEqual(f1.read(), "OLD")
        with open(self.log_path, "r", encoding="utf-8") as lf:
            content = lf.read()
        self.assertTrue(content.startswith("--- engine start "))
        self.assertNotIn("OLD", content)

    def test_existing_1_and_log(self):
        with open(self.log_path_1, "w", encoding="utf-8") as f:
            f.write("ANCIENT")
        with open(self.log_path, "w", encoding="utf-8") as f:
            f.write("NEWEST_OLD")
        f = engine._open_log(self.log_path)
        f.close()
        with open(self.log_path_1, "r", encoding="utf-8") as f1:
            self.assertEqual(f1.read(), "NEWEST_OLD")
        with open(self.log_path, "r", encoding="utf-8") as lf:
            content = lf.read()
        self.assertTrue(content.startswith("--- engine start "))
        self.assertNotIn("NEWEST_OLD", content)

    def test_rotation_fails_appends(self):
        with open(self.log_path, "w", encoding="utf-8") as f:
            f.write("KEEP_ME\n")
        with mock.patch("os.replace", side_effect=OSError("locked")):
            f = engine._open_log(self.log_path)
            f.close()
        with open(self.log_path, "r", encoding="utf-8") as lf:
            content = lf.read()
        self.assertTrue(content.startswith("KEEP_ME\n--- engine start "))

    def test_end_to_end_crash_then_restart(self):
        settings = config.Settings(
            client_id="cid",
            client_secret="",
            redirect_uri="",
            token_path="",
            ascii=True,
            engine=True,
            engine_name="Test",
            engine_bitrate=160,
            engine_bin=sys.executable,  # Stand-in that fails running "python run --cache ..."
        )
        eng = engine.Engine(settings)
        # First start: fails because python unknown command 'run'
        eng.start()
        self.assertIsNotNone(eng._proc)
        eng._proc.wait(5)
        # Wait a tick so timestamps advance if needed
        time.sleep(0.05)
        # Second start: rotates the previous crash log into engine.log.1
        eng.start()
        self.assertIsNotNone(eng._proc)
        eng._proc.wait(5)
        eng.stop()

        self.assertTrue(os.path.exists(self.log_path_1), "engine.log.1 must exist after second start")
        with open(self.log_path_1, "r", encoding="utf-8", errors="replace") as f1:
            content1 = f1.read()
        self.assertTrue(len(content1) > 0, "engine.log.1 should contain first run stderr")
        with open(self.log_path, "r", encoding="utf-8", errors="replace") as f:
            content2 = f.read()
        self.assertIn("--- engine start ", content2)


if __name__ == "__main__":
    unittest.main()
