import unittest

from pages import paginate


class PaginateTest(unittest.TestCase):
    def test_even(self):
        self.assertEqual(paginate([1, 2, 3, 4], 2), [[1, 2], [3, 4]])


if __name__ == "__main__":
    unittest.main()
