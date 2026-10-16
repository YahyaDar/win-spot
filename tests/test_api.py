"""Unit tests for spoterm.api."""

import unittest

from spoterm import api


class FakeEngine:
    def __init__(self, responses=None):
        self.responses = list(responses or [])
        self.calls = []

    def call(self, method, path, params=None, body=None):
        self.calls.append((method, path, params, body))
        if self.responses:
            resp = self.responses.pop(0)
            if isinstance(resp, Exception):
                raise resp
            return resp
        return {}


class TestApi(unittest.TestCase):
    # ── _num ────────────────────────────────────────────────────────────────
    def test_num(self):
        self.assertEqual(api._num(42), 42)
        self.assertEqual(api._num(42.9), 42)
        self.assertEqual(api._num("123"), 123)
        self.assertEqual(api._num(None), 0)
        self.assertEqual(api._num(None, default=10), 10)
        self.assertEqual(api._num("garbage"), 0)
        self.assertEqual(api._num(True), 1)
        self.assertEqual(api._num(False), 0)

    # ── _track ──────────────────────────────────────────────────────────────
    def test_track_invalid_inputs(self):
        self.assertIsNone(api._track(None, 0))
        self.assertIsNone(api._track("not a dict", 0))
        self.assertIsNone(api._track({"name": "No URI"}, 0))
        self.assertIsNone(api._track({"uri": 12345}, 0))

    def test_track_minimal_valid(self):
        t = api._track({"uri": "spotify:track:abc1234"}, 0)
        self.assertIsNotNone(t)
        self.assertEqual(t.uri, "spotify:track:abc1234")
        self.assertEqual(t.id, "abc1234")
        self.assertTrue(t.is_track)
        self.assertEqual(t.name, "Unknown")
        self.assertEqual(t.artists, "")
        self.assertEqual(t.album, "")
        self.assertEqual(t.duration_ms, 0)
        self.assertEqual(t.pos, 0)
        self.assertTrue(t.playable)

    def test_track_linked_from(self):
        item_linked = {
            "uri": "spotify:track:regional",
            "linked_from": {"uri": "spotify:track:original"},
        }
        t = api._track(item_linked, 1)
        self.assertEqual(t.uri, "spotify:track:original")

        item_non_spotify = {
            "uri": "spotify:track:regional",
            "linked_from": {"uri": "https://spotify.com/track/123"},
        }
        t2 = api._track(item_non_spotify, 1)
        self.assertEqual(t2.uri, "spotify:track:regional")

    def test_track_episode(self):
        ep = {
            "uri": "spotify:episode:ep1",
            "type": "episode",
            "name": "Episode 1",
            "show": {"name": "Podcast Name"},
        }
        t = api._track(ep, 0)
        self.assertEqual(t.artists, "Podcast Name")
        self.assertEqual(t.album, "")
        self.assertFalse(t.is_track)

    def test_track_artists_and_album(self):
        item = {
            "uri": "spotify:track:t1",
            "name": "Track Name",
            "artists": [{"name": "Artist 1"}, "skip_me", None, {"name": "Artist 2"}],
            "album": {"name": "Album Name"},
            "duration_ms": "300000",
        }
        t = api._track(item, 2)
        self.assertEqual(t.artists, "Artist 1, Artist 2")
        self.assertEqual(t.album, "Album Name")
        self.assertEqual(t.duration_ms, 300000)

    def test_track_playable_flags(self):
        base = {"uri": "spotify:track:1"}
        self.assertFalse(api._track(base, 0, is_local=True).playable)
        self.assertFalse(api._track({**base, "is_local": True}, 0).playable)
        self.assertFalse(api._track({**base, "is_playable": False}, 0).playable)
        self.assertTrue(api._track({**base, "is_playable": True}, 0).playable)

    def test_track_duration_and_name_fallbacks(self):
        neg = api._track({"uri": "spotify:track:1", "duration_ms": -500}, 0)
        self.assertEqual(neg.duration_ms, 0)

        none_name = api._track({"uri": "spotify:track:1", "name": None}, 0)
        self.assertEqual(none_name.name, "Unknown")
        empty_name = api._track({"uri": "spotify:track:1", "name": ""}, 0)
        self.assertEqual(empty_name.name, "Unknown")

    # ── Spotify._page ───────────────────────────────────────────────────────
    def test_page_parsing(self):
        # Non-list items
        p1 = api.Spotify._page({"items": None, "total": 0}, 0, lambda i: i)
        self.assertEqual(p1.tracks, [])
        self.assertEqual(p1.total, 0)
        self.assertIsNone(p1.next_offset)

        # Non-dict items skipped, pos offset preserved
        raw = {
            "items": ["garbage", None, {"uri": "spotify:track:1", "name": "One"}],
            "total": 10,
            "next": "https://api.spotify.com/v1/next",
        }
        p2 = api.Spotify._page(raw, 5, lambda i: i)
        self.assertEqual(len(p2.tracks), 1)
        self.assertEqual(p2.tracks[0].pos, 7)  # offset (5) + index (2)
        self.assertEqual(p2.total, 10)
        self.assertEqual(p2.next_offset, 8)  # 5 + len(items)(3)

        # Total is at least offset + len(items)
        p3 = api.Spotify._page({"items": [{"uri": "spotify:track:1"}], "total": 0}, 5, lambda i: i)
        self.assertEqual(p3.total, 6)

        # next_offset is None if next is falsy or items empty
        p4 = api.Spotify._page({"items": [], "next": "url"}, 0, lambda i: i)
        self.assertIsNone(p4.next_offset)
        p5 = api.Spotify._page({"items": [{"uri": "spotify:track:1"}], "next": None}, 0, lambda i: i)
        self.assertIsNone(p5.next_offset)

        # Playlist item versus track key
        pl_raw = {
            "items": [
                {"item": {"uri": "spotify:track:item_key"}},
                {"track": {"uri": "spotify:track:track_key"}},
            ]
        }
        p6 = api.Spotify._page(pl_raw, 0, lambda i: i.get("item") or i.get("track"))
        self.assertEqual(len(p6.tracks), 2)
        self.assertEqual(p6.tracks[0].uri, "spotify:track:item_key")
        self.assertEqual(p6.tracks[1].uri, "spotify:track:track_key")

    # ── playlists() ─────────────────────────────────────────────────────────
    def test_playlists(self):
        page1 = {
            "items": [
                {"id": "valid1", "uri": "spotify:playlist:valid1", "name": "PL1", "tracks": {"total": 10}},
                {"id": "a/b", "uri": "spotify:playlist:invalid", "name": "Invalid ID"},
                {"id": "a b", "uri": "spotify:playlist:invalid", "name": "Invalid Space"},
                {"id": "", "uri": "spotify:playlist:invalid", "name": "Empty ID"},
                {"id": "valid2", "uri": "", "name": "Missing URI"},
            ],
            "next": "https://api.spotify.com/v1/me/playlists?offset=50",
        }
        page2 = {
            "items": [
                {"id": "valid3", "uri": "spotify:playlist:valid3", "name": "PL3", "items": {"total": 5}},
            ],
            "next": None,
        }
        engine = FakeEngine([page1, page2])
        sp = api.Spotify(engine)
        pls = sp.playlists()

        self.assertEqual(len(pls), 2)
        self.assertEqual(pls[0].id, "valid1")
        self.assertEqual(pls[0].total, 10)
        self.assertEqual(pls[1].id, "valid3")
        self.assertEqual(pls[1].total, 5)

        # Second call followed next url
        self.assertEqual(len(engine.calls), 2)
        self.assertEqual(engine.calls[0][1], "me/playlists")
        self.assertEqual(engine.calls[1][1], "https://api.spotify.com/v1/me/playlists?offset=50")

    # ── call contracts ──────────────────────────────────────────────────────
    def test_call_contracts(self):
        engine = FakeEngine([
            {"items": []},  # liked_page
            {"items": []},  # playlist_page
            {"tracks": {"items": []}},  # search_page
            [True],  # is_liked
            {},  # is_liked non-list reply
            {},  # set_liked like
            {},  # set_liked unlike
            {},  # playback empty dict
            None,  # playback None
            "bad",  # playback non-dict
            {  # playback valid
                "item": {"uri": "spotify:track:t1"},
                "is_playing": True,
                "progress_ms": 1000,
                "device": {"id": "d1", "name": "Speaker", "volume_percent": 80, "supports_volume": True},
            },
            {  # playback volume None if missing or unsupported
                "device": {"volume_percent": None, "supports_volume": True},
            },
            {  # playback volume None if supports_volume False
                "device": {"volume_percent": 80, "supports_volume": False},
            },
        ])
        sp = api.Spotify(engine)

        # liked_page
        sp.liked_page(10)
        self.assertEqual(engine.calls[-1], ("GET", "me/tracks", {"limit": 50, "offset": 10, "market": "from_token"}, None))

        # playlist_page
        sp.playlist_page("pl123", 20)
        expected_fields = api._PLAYLIST_FIELDS
        self.assertEqual(
            engine.calls[-1],
            (
                "GET",
                "playlists/pl123/items",
                {
                    "limit": 100,
                    "offset": 20,
                    "fields": expected_fields,
                    "additional_types": "track,episode",
                    "market": "from_token",
                },
                None,
            ),
        )

        # search_page
        sp.search_page("rock", 0)
        self.assertEqual(engine.calls[-1], ("GET", "search", {"q": "rock", "type": "track", "limit": 10, "offset": 0}, None))

        # is_liked
        self.assertTrue(sp.is_liked("trk1"))
        self.assertEqual(engine.calls[-1], ("GET", "me/library/contains", {"uris": "spotify:track:trk1"}, None))
        # non-list reply handled
        self.assertFalse(sp.is_liked("trk2"))

        # set_liked
        sp.set_liked("trk1", True)
        self.assertEqual(engine.calls[-1], ("PUT", "me/library", {"uris": "spotify:track:trk1"}, None))
        sp.set_liked("trk1", False)
        self.assertEqual(engine.calls[-1], ("DELETE", "me/library", {"uris": "spotify:track:trk1"}, None))

        # playback() returns None for empty dict, None, non-dict
        self.assertIsNone(sp.playback())
        self.assertIsNone(sp.playback())
        self.assertIsNone(sp.playback())

        # playback() valid
        pb = sp.playback()
        self.assertIsNotNone(pb)
        self.assertEqual(pb.volume, 80)
        self.assertTrue(pb.is_playing)

        # playback volume None
        pb_novol = sp.playback()
        self.assertIsNone(pb_novol.volume)
        pb_unsupported = sp.playback()
        self.assertIsNone(pb_unsupported.volume)

    # ── Playback progress & rebase ──────────────────────────────────────────
    def test_playback_progress_and_rebase(self):
        track = api.Track(uri="spotify:track:1", name="T", artists="", album="", duration_ms=10000, pos=0)
        pb = api.Playback(
            track=track,
            is_playing=True,
            progress_ms=2000,
            fetched_at=100.0,
            device_id="d1",
            device_name="Dev",
            volume=50,
            shuffle=False,
            repeat="off",
            context_uri=None,
        )

        # Playing advances by elapsed time
        self.assertEqual(pb.progress(102.5), 4500)

        # Clamps to duration
        self.assertEqual(pb.progress(120.0), 10000)

        # Paused does not advance
        pb.is_playing = False
        self.assertEqual(pb.progress(105.0), 2000)

        # Unknown duration (0) does not clamp
        track.duration_ms = 0
        pb.is_playing = True
        self.assertEqual(pb.progress(120.0), 22000)

        # Negative progress_ms becomes 0
        pb.progress_ms = -500
        pb.is_playing = False
        self.assertEqual(pb.progress(100.0), 0)

        # Rebase freezes interpolated progress and updates fetched_at
        pb.progress_ms = 1000
        pb.fetched_at = 10.0
        pb.is_playing = True
        pb.rebase(12.0)
        self.assertEqual(pb.progress_ms, 3000)
        self.assertEqual(pb.fetched_at, 12.0)

    # ── describe_error ──────────────────────────────────────────────────────
    def test_describe_error(self):
        # NoDeviceError
        self.assertEqual(api.describe_error(api.NoDeviceError()), "No Spotify device found. Open Spotify somewhere, or press d")
        self.assertEqual(api.describe_error(api.NoDeviceError("Custom msg")), "Custom msg")

        # ApiError login=True
        self.assertEqual(api.describe_error(api.ApiError(401, "msg", login=True)), "Your Spotify sign-in expired: restart SpoTerm to sign in again")

        # ApiError status 0
        self.assertEqual(api.describe_error(api.ApiError(0, "Custom network err")), "Custom network err")
        self.assertEqual(api.describe_error(api.ApiError(0, "")), "Network error: can't reach Spotify")

        # 404 with device
        self.assertEqual(api.describe_error(api.ApiError(404, "Device not found")), "No active device. Press d to pick one")

        # 403 with premium
        self.assertEqual(api.describe_error(api.ApiError(403, "Premium required")), "Spotify Premium is required for playback control")
        self.assertEqual(api.describe_error(api.ApiError(403, "Forbidden", reason="Requires premium")), "Spotify Premium is required for playback control")

        # other 403
        self.assertEqual(api.describe_error(api.ApiError(403, "Forbidden")), "Not allowed: Forbidden")

        # 429
        self.assertEqual(api.describe_error(api.ApiError(429, "Too Many Requests")), "Rate limited by Spotify, slowing down")

        # Generic status extracts text after the last colon
        self.assertEqual(api.describe_error(api.ApiError(500, "Error: Server failure")), "Server failure")

        # TimeoutError
        self.assertEqual(api.describe_error(TimeoutError("timed out")), "Spotify took too long to respond")

        # OSError
        self.assertEqual(api.describe_error(OSError("connection refused")), "Network error: can't reach Spotify")

        # Arbitrary exception
        self.assertEqual(api.describe_error(RuntimeError("something bad")), "RuntimeError: something bad")


if __name__ == "__main__":
    unittest.main()
