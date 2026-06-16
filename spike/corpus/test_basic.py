"""Phase-1 spike corpus — order-independent pytest functions (differential vs pytest).

These outcomes must agree between stock pytest and the Wellspring engine, so they are
deliberately independent of execution order and of each other.
"""
import numpy as np


def test_addition_passes():
    assert 1 + 1 == 2


def test_numpy_sum_passes():
    # Exercises the numpy C-extension INSIDE the forked child (the fork-from-warm target).
    assert int(np.arange(5).sum()) == 10


def test_subtraction_fails():
    # Intentional failure — both pytest and the engine must report FAILED.
    assert 5 - 3 == 1
