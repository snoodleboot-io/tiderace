"""Phase-1 spike corpus — a stdlib unittest.TestCase (driven via TestCase.run(), NO pytest)."""
import unittest


class ArithmeticCase(unittest.TestCase):
    def test_mul_passes(self):
        self.assertEqual(6 * 7, 42)

    def test_div_fails(self):
        # Intentional failure — both pytest and the engine must report FAILED.
        self.assertEqual(10 / 2, 6)
