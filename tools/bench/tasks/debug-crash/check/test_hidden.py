import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


def run(folder):
    p = subprocess.run([sys.executable, "report.py", str(folder)], capture_output=True, text=True, timeout=60)
    return p.returncode, p.stdout


class Hidden(unittest.TestCase):
    def test_given_data(self):
        code, out = run("data")
        self.assertEqual(code, 0, out)
        lines = {l.split(":")[0]: l for l in out.splitlines() if ":" in l}
        self.assertIn("january.csv", lines)
        self.assertIn("35.00", lines["january.csv"])
        self.assertIn("february.csv", lines)
        self.assertIn("20.00", lines["february.csv"])
        self.assertRegex(lines["february.csv"].split("20.00", 1)[1], r"\b2\b")  # two rows skipped
        self.assertIn("march.csv", lines)

    def test_other_folder(self):
        with tempfile.TemporaryDirectory() as d:
            Path(d, "a.csv").write_text("id,amount\n1,abc\n2,3\n")
            Path(d, "b.csv").write_text("id,amount\n")
            code, out = run(d)
            self.assertEqual(code, 0, out)
            self.assertIn("a.csv", out)
            self.assertIn("b.csv", out)
