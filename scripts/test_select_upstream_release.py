import unittest

from scripts.select_upstream_release import select_release, version


class SelectUpstreamReleaseTests(unittest.TestCase):
    def test_newer_stable_release(self):
        self.assertEqual(
            select_release("v0.9.5", {"tag_name": "v0.9.6", "draft": False, "prerelease": False}),
            "v0.9.6",
        )

    def test_current_and_older_release(self):
        for tag in ("v0.9.5", "v0.9.4", "v0.8.99"):
            with self.subTest(tag=tag):
                self.assertIsNone(select_release("v0.9.5", {"tag_name": tag}))

    def test_semantic_not_lexical_order(self):
        self.assertGreater(version("v0.10.0"), version("v0.9.99"))

    def test_rejects_prereleases_drafts_and_unsafe_tags(self):
        for release in (
            {"tag_name": "v0.9.6", "draft": True},
            {"tag_name": "v0.9.6", "prerelease": True},
            {"tag_name": "v0.9.6-rc.1"},
            {"tag_name": "v0.9.6; echo bad"},
        ):
            with self.subTest(release=release), self.assertRaises(ValueError):
                select_release("v0.9.5", release)

    def test_invalid_baseline(self):
        with self.assertRaises(ValueError):
            select_release("untracked", {"tag_name": "v0.9.6"})


if __name__ == "__main__":
    unittest.main()
