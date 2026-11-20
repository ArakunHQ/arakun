import unittest

from durations import parse_duration


class Hidden(unittest.TestCase):
    def test_units(self):
        self.assertEqual(parse_duration("45s"), 45)
        self.assertEqual(parse_duration("90m"), 5400)
        self.assertEqual(parse_duration("1h30m"), 5400)
        self.assertEqual(parse_duration("2d 4h"), 2 * 86400 + 4 * 3600)
        self.assertEqual(parse_duration("1d2h3m4s"), 86400 + 7200 + 180 + 4)

    def test_case_and_space(self):
        self.assertEqual(parse_duration("1H 30M"), 5400)
        self.assertEqual(parse_duration("  10s "), 10)

    def test_bare_number(self):
        self.assertEqual(parse_duration("120"), 120)

    def test_errors(self):
        for bad in ["", "   ", "5x", "1m1h", "1h1h", "-5s", "h", "1.5h"]:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                parse_duration(bad)
