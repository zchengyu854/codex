import subprocess
import sys
import unittest
from report import render


class ReportTests(unittest.TestCase):
    def test_both_features(self):
        self.assertEqual(render(" Hello\t World ", ["2", "-1", "4"]), "hello-world:5")

    def test_cli(self):
        result = subprocess.run(
            [sys.executable, "-B", "report.py", " Hello\t World ", "2", "-1", "4"],
            capture_output=True, text=True, check=True,
        )
        self.assertEqual(result.stdout, "hello-world:5\n")

    def test_cli_rejects_invalid_number(self):
        result = subprocess.run(
            [sys.executable, "-B", "report.py", "title", "invalid"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("values must be integers", result.stderr)
