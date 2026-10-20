import unittest
from slug import slug


class SlugTests(unittest.TestCase):
    def test_words(self):
        self.assertEqual(slug(" Hello World "), "hello-world")

    def test_empty(self):
        self.assertEqual(slug(""), "")

    def test_repeated_whitespace(self):
        self.assertEqual(slug(" Hello\t  World\n "), "hello-world")

    def test_unicode(self):
        self.assertEqual(slug("项目  名称"), "项目-名称")
