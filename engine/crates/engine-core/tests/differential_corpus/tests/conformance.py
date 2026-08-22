"""A shared contract suite subclassed per backend — the shape that hid 129 tests (TID-26).

Nothing here appears in the files that subclass it, so a source scan sees classes with no
test methods.
"""

import unittest


class StoreConformance:
    """Tests live only here; subclasses supply `make_store`."""

    def make_store(self):
        raise NotImplementedError

    def test_round_trips(self):
        assert self.make_store() == self.expected

    def test_is_not_empty(self):
        assert self.make_store()


class _SharedCase(unittest.TestCase):
    """An intermediate base, so subclasses are `TestCase` subclasses transitively."""

    def helper(self):
        return "shared"
