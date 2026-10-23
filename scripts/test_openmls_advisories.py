"""Tests for scripts/openmls_advisories.py. Run: python scripts/test_openmls_advisories.py"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import openmls_advisories as w


def adv(ghsa, rng, severity="high"):
    return {"ghsa_id": ghsa, "severity": severity, "summary": "x", "vulnerabilities": [{"vulnerable_version_range": rng}]}


class Ranges(unittest.TestCase):
    def test_bounds(self):
        self.assertTrue(w.affected("0.9.1", ">= 0.9.0, < 0.9.2"))
        self.assertFalse(w.affected("0.9.2", ">= 0.9.0, < 0.9.2"), "fixed in 0.9.2 itself")
        self.assertFalse(w.affected("0.8.9", ">= 0.9.0, < 0.9.2"))
        self.assertTrue(w.affected("0.7.0", "0.7.0"), "a bare version means equal")
        self.assertFalse(w.affected("0.7.1", "0.7.0"))
        self.assertTrue(w.affected("0.5.0", "< 0.9.0"))
        self.assertTrue(w.affected("0.9.10", "> 0.9.2"), "numeric, not textual, comparison")

    def test_what_cannot_be_read_counts_as_affected(self):
        self.assertTrue(w.affected("0.9.2", ""))
        self.assertTrue(w.affected("0.9.2", ">= 0.9, < 1"))
        self.assertTrue(w.affected("0.9.2", "some words"))


class Verdicts(unittest.TestCase):
    def test_a_fixed_and_reviewed_advisory_is_fine(self):
        self.assertEqual(w.check("0.9.2", [adv("GHSA-a", "< 0.9.0")], {"GHSA-a"}), [])

    def test_an_affecting_advisory_is_a_problem_even_if_reviewed(self):
        p = w.check("0.9.2", [adv("GHSA-a", ">= 0.9.0, < 0.9.3")], {"GHSA-a"})
        self.assertEqual(len(p), 1)
        self.assertIn("AFFECTS", p[0])

    def test_a_new_advisory_must_be_read_by_a_person(self):
        p = w.check("0.9.2", [adv("GHSA-new", "< 0.9.0")], {"GHSA-a"})
        self.assertEqual(len(p), 1)
        self.assertIn("NOT REVIEWED", p[0])

    def test_the_review_list_ignores_comments_and_notes(self):
        self.assertEqual(w.reviewed_ids("# comment\n\nGHSA-a  fixed 0.8.3; we pin 0.9.2\n"), {"GHSA-a"})

    def test_the_pin_must_be_exact(self):
        self.assertEqual(w.pinned_version('openmls = { version = "=0.9.2", default-features = false }'), "0.9.2")
        with self.assertRaises(SystemExit):
            w.pinned_version('openmls = { version = "0.9.2" }')
        with self.assertRaises(SystemExit):
            w.pinned_version('openmls = "0.9"')


if __name__ == "__main__":
    unittest.main()
