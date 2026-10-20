import unittest
from slug import slug


class SlugTests(unittest.TestCase):
    def test_words(self):
        self.assertEqual(slug(" Hello World "), "hello-world")

    def test_empty(self):
        self.assertEqual(slug(""), "")
