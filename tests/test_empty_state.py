import unittest
import os

try:
    import curses  # noqa: F401
except ImportError:
    raise unittest.SkipTest("curses not available (pip install windows-curses)")

from spoterm import app


class TestEmptyState(unittest.TestCase):
    def test_key_reload_constant(self):
        # KEY_RELOAD must equal "r"
        self.assertTrue(hasattr(app, "KEY_RELOAD"), "KEY_RELOAD constant missing in spoterm.app")
        self.assertEqual(app.KEY_RELOAD, "r")

        # Verify README key table says r for reload
        readme_path = os.path.join(os.path.dirname(__file__), "..", "README.md")
        with open(readme_path, "r", encoding="utf-8") as f:
            content = f.read()
        self.assertIn("`r` | Reload", content)

    def test_empty_state_error(self):
        tl = app.TrackList("k", "playlist", "T")
        tl.error = "boom"
        msg, hint, role = app.empty_state(tl, typing=False, ell="...")
        self.assertEqual(msg, "boom")
        self.assertIn("press r to retry", hint)
        self.assertNotIn("press R to retry", hint)
        self.assertEqual(role, app.WARN)

    def test_empty_state_locked(self):
        tl = app.TrackList("k", "playlist", "T")
        tl.locked = True
        msg, hint, role = app.empty_state(tl, typing=False, ell="...")
        self.assertEqual(msg, "Spotify doesn't let apps list this playlist")
        self.assertEqual(hint, "press enter to play it")
        self.assertEqual(role, app.DIM)

    def test_empty_state_loading(self):
        tl = app.TrackList("k", "playlist", "T")
        tl.loading = True
        msg, hint, role = app.empty_state(tl, typing=False, ell="…")
        self.assertEqual(msg, "Loading…")
        self.assertEqual(hint, "")
        self.assertEqual(role, app.DIM)

    def test_empty_state_search_empty_query(self):
        tl = app.TrackList("search", "search", "Search")
        tl.query = ""

        # Not typing
        msg, hint, role = app.empty_state(tl, typing=False, ell="...")
        self.assertEqual(msg, "Search Spotify")
        self.assertEqual(hint, "press / to start")
        self.assertEqual(role, app.DIM)

        # Typing
        msg, hint, role = app.empty_state(tl, typing=True, ell="...")
        self.assertEqual(msg, "Search Spotify")
        self.assertEqual(hint, "type, then press enter")
        self.assertEqual(role, app.DIM)

    def test_empty_state_search_no_results(self):
        tl = app.TrackList("search", "search", "Search")
        tl.query = "my query"
        msg, hint, role = app.empty_state(tl, typing=False, ell="...")
        self.assertEqual(msg, 'No results for "my query"')
        self.assertEqual(hint, "try different words")
        self.assertEqual(role, app.DIM)

    def test_empty_state_plain_empty(self):
        tl = app.TrackList("k", "liked", "Liked")
        msg, hint, role = app.empty_state(tl, typing=False, ell="...")
        self.assertEqual(msg, "Nothing here")
        self.assertEqual(hint, "")
        self.assertEqual(role, app.DIM)

    def test_empty_state_precedence(self):
        # locked beats error beats loading
        tl = app.TrackList("k", "playlist", "T")
        tl.locked = True
        tl.error = "network failed"
        tl.loading = True
        msg, hint, role = app.empty_state(tl, typing=False, ell="...")
        self.assertEqual(msg, "Spotify doesn't let apps list this playlist")

        tl.locked = False
        msg, hint, role = app.empty_state(tl, typing=False, ell="...")
        self.assertEqual(msg, "network failed")

        tl.error = ""
        msg, hint, role = app.empty_state(tl, typing=False, ell="...")
        self.assertEqual(msg, "Loading...")


if __name__ == "__main__":
    unittest.main()
