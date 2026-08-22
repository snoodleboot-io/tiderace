"""Collection shapes a source scan cannot see (TID-26)."""

import unittest

from conformance import StoreConformance, _SharedCase


class TestMemoryBackend(StoreConformance):
    """Every test inherited from another module."""

    expected = "mem"

    def make_store(self):
        return "mem"


class TestDiskBackend(StoreConformance):
    """Inherited tests AND one of its own — neither may be lost or doubled."""

    expected = "disk"

    def make_store(self):
        return "disk"

    def test_own_method_too(self):
        assert True


class PackOverridesBuiltinTests(_SharedCase):
    """A `TestCase` subclass transitively, whose name pytest's `Test*` rule does not match."""

    def test_transitive_unittest_subclass(self):
        assert self.helper() == "shared"


class TestNestedClassInABody(unittest.TestCase):
    def test_defines_a_local_class(self):
        class ReadTimeout(Exception):
            pass

        assert issubclass(ReadTimeout, Exception)

    def test_after_the_nested_class(self):
        """Dropped when a nested class closed the enclosing one."""
        assert True


class TestSourceInAString(unittest.TestCase):
    SNIPPET = '''
class P:
    async def process(self, x):
        return x
'''

    def test_after_the_string_literal(self):
        """Dropped when `class P:` inside the literal read as real code."""
        assert "class P" in self.SNIPPET


class NotATestClass:
    """No `Test` prefix and not a `TestCase` — pytest ignores it, so we must too."""

    def test_never_collected(self):
        raise AssertionError("this class must not be collected")
