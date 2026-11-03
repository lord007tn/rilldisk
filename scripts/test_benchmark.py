# SPDX-License-Identifier: AGPL-3.0-only
"""Regression checks for independent synthetic-fixture qualification."""
import contextlib
import copy
import io
import types
import unittest

from benchmark import COUNTS, summarize


class ManifestGateTests(unittest.TestCase):
    def setUp(self):
        manifest = dict(zip(COUNTS, (10, 4, 100, 80, 2, 1, 0, 0)))
        manifest["unreadable"] = dict(files=2, directories=1, logical_bytes=20)
        row = dict(manifest)
        row.pop("unreadable")
        row.update(backend="ntfs-mft", requested_backend="mft", threads=1,
                   run=0, wall_ms=1, files_per_s=10000, peak_rss_bytes=1024,
                   peak_commit_bytes=1024, allocated_bytes=4096, complete=True)
        self.report = dict(platform="synthetic", cpus=1, elevated=True,
                           target="fictional", runs=[row], image_runs=[],
                           backends=["mft"], manifest=manifest, truth=None)
        self.args = types.SimpleNamespace(cold_first=False, require_identical=True)

    def gate(self):
        with contextlib.redirect_stdout(io.StringIO()):
            return summarize(self.report, self.args)

    def test_known_manifest_passes(self):
        self.assertTrue(self.gate())

    def test_all_backends_agreeing_on_wrong_counts_fails(self):
        self.report["runs"][0]["files"] = 9
        other = copy.deepcopy(self.report["runs"][0])
        other.update(backend="nt-directory", requested_backend="directory")
        self.report["runs"].append(other)
        self.report["backends"].append("directory")
        self.assertFalse(self.gate())

    def test_known_locked_folder_coverage_passes(self):
        self.report["runs"][0].update(files=8, directories=3, logical_bytes=80,
                                       unique_logical_bytes=60, issue_count=1, complete=False)
        self.assertTrue(self.gate())

    def test_unknown_missing_folder_fails(self):
        self.report["runs"][0].update(files=7, issue_count=1)
        self.assertFalse(self.gate())

    def test_incomplete_result_with_matching_totals_fails(self):
        self.report["runs"][0]["complete"] = False
        self.assertFalse(self.gate())

    def test_real_tree_without_manifest_remains_comparison_only(self):
        self.report["manifest"] = None
        self.assertTrue(self.gate())


if __name__ == "__main__":
    unittest.main()
