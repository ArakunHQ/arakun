import pathlib
import re
import unittest

from shop import pricing
from shop.cart import Cart
from shop.report import summary


class Hidden(unittest.TestCase):
    def test_new_name(self):
        self.assertEqual(pricing.order_total([(10.0, 2), (5.0, 1)]), 25.0)
        self.assertEqual(pricing.order_total([(10.0, 1)], discount=0.5), 5.0)
        self.assertFalse(hasattr(pricing, "calc"))

    def test_callers(self):
        c = Cart()
        c.add(10.0, 3)
        self.assertEqual(c.total(member=True), 27.0)
        self.assertEqual(c.total(), 30.0)
        self.assertEqual(summary([[(1.0, 1)], [(2.5, 2)]]), ["#1: 1.00", "#2: 5.00"])

    def test_no_old_name(self):
        for f in pathlib.Path(".").rglob("*.py"):
            if "bench_hidden" in f.parts:
                continue
            self.assertIsNone(re.search(r"\bcalc\b", f.read_text()), f)
