import unittest

from shop.cart import Cart
from shop.pricing import calc
from shop.report import summary


class ShopTest(unittest.TestCase):
    def test_calc(self):
        self.assertEqual(calc([(10.0, 2), (5.0, 1)]), 25.0)
        self.assertEqual(calc([(10.0, 1)], d=0.5), 5.0)

    def test_cart(self):
        c = Cart()
        c.add(10.0, 3)
        self.assertEqual(c.total(member=True), 27.0)

    def test_summary(self):
        self.assertEqual(summary([[(1.0, 1)]]), ["#1: 1.00"])


if __name__ == "__main__":
    unittest.main()
