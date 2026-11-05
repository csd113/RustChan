"""Keep the historical Windows baseline exception out of candidate checks."""

from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import binary_upgrade as upgrade


class Responses:
    def __init__(self):
        self.media_status = 404

    def request(self, route):
        if route == "/test/thread/1":
            return 200, {}, b"upgrade-retained-original upgrade-retained-reply"
        if route == "/boards/test/fixture.png":
            return self.media_status, {}, b"fixture"
        if route == "/banner/assets/1":
            return 200, {}, b"banner"
        if route == "/private":
            return 403, {}, b"/private/unlock"
        raise AssertionError("unexpected fixture route: " + route)


class BaselineContract(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.data = Path(self.directory.name)
        media = self.data / "boards" / "test" / "fixture.png"
        media.parent.mkdir(parents=True)
        media.write_bytes(b"fixture")
        self.smoke = upgrade.Smoke.__new__(upgrade.Smoke)
        self.smoke.media = b"fixture"
        self.smoke.media_name = "fixture.png"
        self.smoke.origin = "http://127.0.0.1"
        self.smoke.baseline_observations = {}
        self.responses = Responses()
        self.banner_hash = upgrade.digest(b"banner")

    def test_old_windows_404_never_excuses_candidate_404(self):
        with patch.object(upgrade, "os", SimpleNamespace(name="nt")), \
                patch.object(upgrade, "Client", return_value=self.responses):
            self.smoke.readable(self.responses, 1, 1, self.banner_hash, legacy_data=self.data)
            self.assertEqual(self.smoke.baseline_observations["windows_171_media_http_status"], 404)
            with self.assertRaisesRegex(upgrade.VerificationError, "HTTP 404"):
                self.smoke.readable(self.responses, 1, 1, self.banner_hash)
            self.responses.media_status = 200
            self.smoke.readable(self.responses, 1, 1, self.banner_hash)

    def test_other_platforms_still_require_readable_baseline_media(self):
        with patch.object(upgrade, "os", SimpleNamespace(name="posix")), \
                patch.object(upgrade, "Client", return_value=self.responses):
            with self.assertRaisesRegex(upgrade.VerificationError, "HTTP 404"):
                self.smoke.readable(self.responses, 1, 1, self.banner_hash, legacy_data=self.data)


if __name__ == "__main__":
    unittest.main()
