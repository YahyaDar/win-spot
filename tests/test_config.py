"""Unit tests for spoterm.config."""

import os
import tempfile
import unittest
from unittest import mock

from spoterm import config


class TestConfig(unittest.TestCase):
    def setUp(self):
        self.tmp_dir = tempfile.TemporaryDirectory()
        self.tmp = self.tmp_dir.name

        # Clean environment: remove SPOTIPY_*, SPOTERM_*, HTTPS_PROXY, NO_PROXY
        clean_env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith(("SPOTIPY_", "SPOTERM_"))
            and k not in ("HTTPS_PROXY", "https_proxy", "NO_PROXY", "no_proxy")
        }
        clean_env["APPDATA"] = os.path.join(self.tmp, "appdata")

        self.env_patcher = mock.patch.dict(os.environ, clean_env, clear=True)
        self.env_patcher.start()

        self.config_dir_patcher = mock.patch.object(config, "config_dir", lambda: self.tmp)
        self.config_dir_patcher.start()

        self.app_dir_patcher = mock.patch.object(config, "APP_DIR", self.tmp)
        self.app_dir_patcher.start()

    def tearDown(self):
        self.app_dir_patcher.stop()
        self.config_dir_patcher.stop()
        self.env_patcher.stop()
        self.tmp_dir.cleanup()

    # ── parse_env basics ────────────────────────────────────────────────────
    def test_parse_env_basics(self):
        text = """
        # A full comment line
           
        export KEY_EXPORT=1
        KEY_SPACES = value with spaces 
        KEY_COMMENT = value # trailing comment
        KEY_HASH = a#b
        """
        env = config.parse_env(text)
        self.assertEqual(env.get("KEY_EXPORT"), "1")
        self.assertEqual(env.get("KEY_SPACES"), "value with spaces")
        self.assertEqual(env.get("KEY_COMMENT"), "value")
        self.assertEqual(env.get("KEY_HASH"), "a#b")
        self.assertNotIn("#", env)

    # ── quoting ─────────────────────────────────────────────────────────────
    def test_parse_env_quoting(self):
        text = r'''
        DOUBLE_ESCAPES="line1\nline2\ttab\rret\"quote\\slash\x"
        SINGLE_LITERAL='literal\n\t'
        DOUBLE_HASH="foo # bar"
        SINGLE_HASH='foo # bar'
        '''
        env = config.parse_env(text)
        self.assertEqual(env.get("DOUBLE_ESCAPES"), 'line1\nline2\ttab\rret"quote\\slash\\x')
        self.assertEqual(env.get("SINGLE_LITERAL"), r"literal\n\t")
        self.assertEqual(env.get("DOUBLE_HASH"), "foo # bar")
        self.assertEqual(env.get("SINGLE_HASH"), "foo # bar")

    # ── malformed ───────────────────────────────────────────────────────────
    def test_parse_env_malformed(self):
        text = """
        UNTERMINATED="unterminated quote
        1DIGIT_KEY=val
        KEY-DASH=val
        KEY SPACE=val
        LINE_WITHOUT_EQUALS
        =EMPTY_KEY
        NUL_VALUE=val\0hidden
        """
        env = config.parse_env(text)
        self.assertNotIn("UNTERMINATED", env)
        self.assertNotIn("1DIGIT_KEY", env)
        self.assertNotIn("KEY-DASH", env)
        self.assertNotIn("KEY SPACE", env)
        self.assertNotIn("LINE_WITHOUT_EQUALS", env)
        self.assertNotIn("", env)
        self.assertNotIn("NUL_VALUE", env)

    # ── duplicates ──────────────────────────────────────────────────────────
    def test_parse_env_duplicates(self):
        # Document today's behavior: last duplicate wins in Python dict
        text = """
        KEY=first
        KEY=second
        """
        env = config.parse_env(text)
        self.assertEqual(env.get("KEY"), "second")

    # ── valid_client_id ─────────────────────────────────────────────────────
    def test_valid_client_id(self):
        self.assertTrue(config.valid_client_id("0123456789abcdef0123456789abcdef"))
        self.assertTrue(config.valid_client_id("0123456789ABCDEF0123456789ABCDEF"))
        self.assertFalse(config.valid_client_id("0" * 31))
        self.assertFalse(config.valid_client_id("0" * 33))
        self.assertFalse(config.valid_client_id("g" * 32))
        self.assertFalse(config.valid_client_id(""))

    # ── save_client_id ──────────────────────────────────────────────────────
    def test_save_client_id_rejects_invalid(self):
        with self.assertRaises(ValueError):
            config.save_client_id("not-a-valid-id")

    def test_save_client_id_success(self):
        cid1 = "a" * 32
        path = config.save_client_id(cid1)
        self.assertTrue(os.path.isfile(path))
        self.assertEqual(os.environ.get("SPOTIPY_CLIENT_ID"), cid1)
        with open(path, encoding="utf-8") as f:
            content = f.read()
        self.assertIn(f"SPOTIPY_CLIENT_ID={cid1}", content)

        # Second call appends again (document current behavior, see BUG-04)
        cid2 = "b" * 32
        config.save_client_id(cid2)
        self.assertEqual(os.environ.get("SPOTIPY_CLIENT_ID"), cid2)
        with open(path, encoding="utf-8") as f:
            content2 = f.read()
        self.assertIn(f"SPOTIPY_CLIENT_ID={cid1}", content2)
        self.assertIn(f"SPOTIPY_CLIENT_ID={cid2}", content2)

    # ── load() ──────────────────────────────────────────────────────────────
    def test_load_missing_client_id(self):
        with self.assertRaises(config.ConfigError) as ctx:
            config.load()
        self.assertIn(self.tmp, str(ctx.exception))

    def test_load_bitrate(self):
        cid = "a" * 32
        os.environ["SPOTIPY_CLIENT_ID"] = cid
        for valid in ("96", "160", "320"):
            os.environ["SPOTERM_BITRATE"] = valid
            settings = config.load()
            self.assertEqual(settings.engine_bitrate, int(valid))
        for invalid in ("128", "256", "foo", ""):
            os.environ["SPOTERM_BITRATE"] = invalid
            settings = config.load()
            self.assertEqual(settings.engine_bitrate, 160)

    def test_load_flags(self):
        cid = "a" * 32
        os.environ["SPOTIPY_CLIENT_ID"] = cid
        for truthy in ("1", "true", "True", "YES", "on", "On"):
            os.environ["SPOTERM_ASCII"] = truthy
            self.assertTrue(config.load().ascii)
        for falsy in ("0", "false", "no", "off", "garbage", ""):
            os.environ["SPOTERM_ASCII"] = falsy
            self.assertFalse(config.load().ascii)

    def test_load_defaults_and_overrides(self):
        cid = "a" * 32
        os.environ["SPOTIPY_CLIENT_ID"] = cid
        settings = config.load()
        self.assertEqual(settings.engine_name, "SpoTerm")

        custom_token = os.path.join(self.tmp, "custom_token.json")
        os.environ["SPOTERM_TOKEN_PATH"] = custom_token
        settings = config.load()
        self.assertEqual(settings.token_path, custom_token)

    def test_load_env_file_in_config_dir(self):
        cid = "a" * 32
        env_file = os.path.join(self.tmp, ".env")
        with open(env_file, "w", encoding="utf-8") as f:
            f.write(f"SPOTIPY_CLIENT_ID={cid}\nSPOTERM_DEVICE_NAME=MyDevice\n")
        settings = config.load()
        self.assertEqual(settings.client_id, cid)
        self.assertEqual(settings.engine_name, "MyDevice")

    def test_load_real_env_wins_over_env_file(self):
        cid1 = "1" * 32
        cid2 = "2" * 32
        env_file = os.path.join(self.tmp, ".env")
        with open(env_file, "w", encoding="utf-8") as f:
            f.write(f"SPOTIPY_CLIENT_ID={cid1}\n")
        os.environ["SPOTIPY_CLIENT_ID"] = cid2
        settings = config.load()
        self.assertEqual(settings.client_id, cid2)

    def test_load_ignores_unwhitelisted_keys_in_env_file(self):
        cid = "a" * 32
        env_file = os.path.join(self.tmp, ".env")
        with open(env_file, "w", encoding="utf-8") as f:
            f.write(f"SPOTIPY_CLIENT_ID={cid}\nSSL_CERT_FILE=/tmp/cert.pem\nPATH=/stray/bin\n")
        config.load()
        self.assertNotIn("SSL_CERT_FILE", os.environ)
        self.assertNotEqual(os.environ.get("PATH"), "/stray/bin")

    # ── BUG-04 ──────────────────────────────────────────────────────────────
    @unittest.expectedFailure
    def test_bug_04_placeholder_client_id_rejected(self):
        # BUG-04: placeholder 'your_client_id' should raise ConfigError.
        os.environ["SPOTIPY_CLIENT_ID"] = "your_client_id"
        with self.assertRaises(config.ConfigError):
            config.load()


if __name__ == "__main__":
    unittest.main()
