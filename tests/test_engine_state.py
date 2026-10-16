"""Unit tests for spoterm.engine state and dispatch."""

import os
import tempfile
import threading
import unittest
from unittest import mock

from spoterm import config, engine


class FakeSettings:
    def __init__(
        self,
        engine_name="SpoTerm",
        engine=True,
        engine_bitrate=160,
        client_id="a" * 32,
        token_path="/tmp/token.json",
        engine_bin="/fake/spoterm-engine",
    ):
        self.engine_name = engine_name
        self.engine = engine
        self.engine_bitrate = engine_bitrate
        self.client_id = client_id
        self.token_path = token_path
        self.engine_bin = engine_bin


class FakeProc:
    def __init__(self, exit_code=None):
        self._exit_code = exit_code

    def poll(self):
        return self._exit_code


class TestEngineState(unittest.TestCase):
    def setUp(self):
        self.tmp_dir = tempfile.TemporaryDirectory()
        self.tmp = self.tmp_dir.name
        self.config_dir_patcher = mock.patch.object(config, "config_dir", lambda: self.tmp)
        self.config_dir_patcher.start()

    def tearDown(self):
        self.config_dir_patcher.stop()
        self.tmp_dir.cleanup()

    # ── _dispatch ───────────────────────────────────────────────────────────
    def test_dispatch_hello(self):
        eng = engine.Engine(FakeSettings())
        self.assertFalse(eng.alive.is_set())
        eng._dispatch({"ev": "hello"})
        self.assertTrue(eng.alive.is_set())
        self.assertTrue(eng.events.empty())  # hello is not queued

    def test_dispatch_ready(self):
        eng = engine.Engine(FakeSettings())
        eng.player_state = "retrying"
        eng.player_error = "some error"
        eng._dispatch({"ev": "ready", "device_id": "dev123"})
        self.assertTrue(eng.ready)
        self.assertEqual(eng.device_id, "dev123")
        self.assertEqual(eng.player_state, "")
        self.assertEqual(eng.player_error, "")
        ev = eng.events.get_nowait()
        self.assertEqual(ev["ev"], "ready")

    def test_dispatch_reconnecting_and_reconnected(self):
        eng = engine.Engine(FakeSettings())
        eng.ready = True
        eng.device_id = "initial_dev"

        # reconnecting
        eng._dispatch({"ev": "reconnecting"})
        self.assertFalse(eng.ready)
        self.assertEqual(eng.player_state, "retrying")
        self.assertEqual(eng.events.get_nowait()["ev"], "reconnecting")

        # reconnected without device_id keeps old device id
        eng._dispatch({"ev": "reconnected"})
        self.assertTrue(eng.ready)
        self.assertEqual(eng.player_state, "")
        self.assertEqual(eng.player_error, "")
        self.assertEqual(eng.device_id, "initial_dev")
        self.assertEqual(eng.events.get_nowait()["ev"], "reconnected")

        # reconnected with new device_id updates it
        eng._dispatch({"ev": "reconnected", "device_id": "new_dev"})
        self.assertEqual(eng.device_id, "new_dev")
        self.assertEqual(eng.events.get_nowait()["ev"], "reconnected")

    def test_dispatch_player_event(self):
        eng = engine.Engine(FakeSettings())

        # state "login"
        eng._dispatch({"ev": "player", "state": "login", "msg": "need login"})
        self.assertEqual(eng.player_state, "login")
        self.assertEqual(eng.player_error, "need login")

        # state "retrying"
        eng._dispatch({"ev": "player", "state": "retrying", "msg": "retrying connection"})
        self.assertEqual(eng.player_state, "retrying")
        self.assertEqual(eng.player_error, "retrying connection")

        # state other normalized to "" and non-string msg normalized to ""
        eng._dispatch({"ev": "player", "state": "unknown_state", "msg": 12345})
        self.assertEqual(eng.player_state, "")
        self.assertEqual(eng.player_error, "")

    def test_dispatch_unknown_event_is_queued(self):
        eng = engine.Engine(FakeSettings())
        eng._dispatch({"ev": "custom_event", "foo": "bar"})
        ev = eng.events.get_nowait()
        self.assertEqual(ev["ev"], "custom_event")
        self.assertEqual(ev["foo"], "bar")

    def test_dispatch_invalid_device_id_ignored(self):
        eng = engine.Engine(FakeSettings())
        eng._dispatch({"ev": "ready", "device_id": 99999})
        self.assertTrue(eng.ready)
        self.assertIsNone(eng.device_id)

    def test_dispatch_api_event_completes_slot(self):
        eng = engine.Engine(FakeSettings())
        event = threading.Event()
        slot = [event, None]
        eng._calls[42] = slot

        eng._dispatch({"ev": "api", "id": 42, "status": 200, "body": {"ok": True}})
        self.assertTrue(event.is_set())
        self.assertEqual(slot[1]["status"], 200)
        self.assertNotIn(42, eng._calls)
        self.assertTrue(eng.events.empty())  # API event not queued to events

    # ── status ──────────────────────────────────────────────────────────────
    def test_status(self):
        # off when player disabled
        eng_off = engine.Engine(FakeSettings(engine=False))
        self.assertEqual(eng_off.status, "off")

        # not built when no binary
        eng_nobin = engine.Engine(FakeSettings(engine_bin=""))
        eng_nobin.binary = None
        self.assertEqual(eng_nobin.status, "not built")

        # login needed
        eng_login = engine.Engine(FakeSettings())
        eng_login.binary = "/fake/bin"
        eng_login.player_state = "login"
        self.assertEqual(eng_login.status, "login needed")

        # running scenarios
        eng_run = engine.Engine(FakeSettings())
        eng_run.binary = "/fake/bin"
        eng_run._proc = FakeProc(None)

        # ready
        eng_run.ready = True
        self.assertEqual(eng_run.status, "ready")

        # retrying
        eng_run.ready = False
        eng_run.player_state = "retrying"
        self.assertEqual(eng_run.status, "retrying")

        # starting
        eng_run.player_state = ""
        self.assertEqual(eng_run.status, "starting")

        # stopped (N)
        eng_stop = engine.Engine(FakeSettings())
        eng_stop.binary = "/fake/bin"
        eng_stop._proc = FakeProc(1)
        eng_stop._exit_code = 1
        self.assertEqual(eng_stop.status, "stopped (1)")

    # ── needs_login ─────────────────────────────────────────────────────────
    def test_needs_login(self):
        cache_dir = os.path.join(self.tmp, "engine")
        os.makedirs(cache_dir, exist_ok=True)
        creds_file = os.path.join(cache_dir, "credentials.json")

        eng = engine.Engine(FakeSettings())
        eng.cache_dir = cache_dir

        # credentials absent -> needs_login True
        self.assertTrue(eng.needs_login())

        # credentials present -> False
        with open(creds_file, "w") as f:
            f.write("{}")
        self.assertFalse(eng.needs_login())

        # player_state "login" forces True even if credentials file present
        eng.player_state = "login"
        self.assertTrue(eng.needs_login())

        # player disabled -> False
        eng.player_enabled = False
        self.assertFalse(eng.needs_login())

    # ── find_binary ─────────────────────────────────────────────────────────
    def test_find_binary(self):
        real_file = os.path.join(self.tmp, "my_engine.exe")
        with open(real_file, "w") as f:
            f.write("")

        self.assertEqual(engine.find_binary(real_file), real_file)
        self.assertIsNone(engine.find_binary(os.path.join(self.tmp, "missing.exe")))

    # ── _env ────────────────────────────────────────────────────────────────
    def test_env(self):
        eng = engine.Engine(FakeSettings())
        with mock.patch.dict(
            os.environ,
            {
                "PATH": "/usr/bin",
                "SPOTIPY_CLIENT_ID": "secret",
                "SPOTERM_BITRATE": "320",
                "CUSTOM_VAR": "val",
            },
            clear=True,
        ):
            env = eng._env()
            self.assertEqual(env.get("PATH"), "/usr/bin")
            self.assertEqual(env.get("CUSTOM_VAR"), "val")
            self.assertNotIn("SPOTIPY_CLIENT_ID", env)
            self.assertNotIn("SPOTERM_BITRATE", env)
            self.assertEqual(env.get("RUST_LOG"), "warn")

        # RUST_LOG kept if already set
        with mock.patch.dict(os.environ, {"PATH": "/usr/bin", "RUST_LOG": "debug"}, clear=True):
            env2 = eng._env()
            self.assertEqual(env2.get("RUST_LOG"), "debug")

    # ── _fail_calls ─────────────────────────────────────────────────────────
    def test_fail_calls(self):
        eng = engine.Engine(FakeSettings())
        ev1, ev2 = threading.Event(), threading.Event()
        slot1 = [ev1, None]
        slot2 = [ev2, None]
        eng._calls = {1: slot1, 2: slot2}

        eng._fail_calls("Engine terminated")
        self.assertTrue(ev1.is_set())
        self.assertTrue(ev2.is_set())
        self.assertEqual(slot1[1], {"ev": "api", "status": 0, "error": "Engine terminated"})
        self.assertEqual(slot2[1], {"ev": "api", "status": 0, "error": "Engine terminated"})
        self.assertEqual(eng._calls, {})


if __name__ == "__main__":
    unittest.main()
