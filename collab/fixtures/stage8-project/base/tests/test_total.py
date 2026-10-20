import unittest
from total import total


class TotalTests(unittest.TestCase):
    def test_numbers(self):
        self.assertEqual(total([2, -1, 4]), 5)

    def test_empty(self):
        self.assertEqual(total([]), 0)
