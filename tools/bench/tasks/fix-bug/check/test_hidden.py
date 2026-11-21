import unittest

from pages import page_count, paginate


class Hidden(unittest.TestCase):
    def test_even(self):
        self.assertEqual(paginate([1, 2, 3, 4], 2), [[1, 2], [3, 4]])

    def test_remainder(self):
        self.assertEqual(paginate([1, 2, 3, 4, 5], 2), [[1, 2], [3, 4], [5]])

    def test_single(self):
        self.assertEqual(paginate([7], 3), [[7]])

    def test_empty(self):
        self.assertEqual(paginate([], 3), [])
        self.assertEqual(page_count([], 3), 0)

    def test_size_one(self):
        self.assertEqual(paginate([1, 2, 3], 1), [[1], [2], [3]])

    def test_bad_size(self):
        with self.assertRaises(ValueError):
            paginate([1], 0)
