"""The three styles and every outcome, plus async."""

import unittest

import pytest


def test_function_passes():
    assert 1 + 1 == 2


def test_function_fails():
    assert 5 - 3 == 1


def test_function_errors():
    raise RuntimeError("boom")


@pytest.mark.skip(reason="skipped by a direct marker")
def test_direct_skip_marker():
    raise AssertionError("must not run")


@pytest.mark.skipif(True, reason="condition is true")
def test_skipif_true():
    raise AssertionError("must not run")


@pytest.mark.skipif(False, reason="condition is false")
def test_skipif_false():
    assert True


def test_importorskip_absent_module():
    pytest.importorskip("a_module_that_cannot_possibly_exist")
    raise AssertionError("must not run")


@pytest.mark.needs_backend
def test_marked_for_the_collection_hook():
    raise AssertionError("the ancestor conftest's hook must skip this")


def test_uses_the_ancestor_fixture(ancestor_fixture):
    assert ancestor_fixture == "from-ancestor"


class TestClassStyle:
    def test_method_passes(self):
        assert "x".upper() == "X"

    def test_method_fails(self):
        assert []


class TestUnittestStyle(unittest.TestCase):
    def test_case_passes(self):
        self.assertEqual(6 * 7, 42)

    def test_case_fails(self):
        self.assertEqual(10 / 2, 6)

    def test_case_skips(self):
        self.skipTest("skipped from inside the case")


class TestAsyncStyle(unittest.IsolatedAsyncioTestCase):
    async def test_async_passes(self):
        assert True

    async def test_async_fails(self):
        assert False
