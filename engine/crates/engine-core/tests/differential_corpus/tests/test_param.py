"""Parametrization ids, which are selectors and must match pytest's spelling (TID-25).

Parametrized FIXTURES are deliberately absent: a fixture's own `ids=` is not read yet, so
`[_in_memory]` is produced where pytest prints `[in_memory]`. Add them here when that lands —
an allowlist of known differences would rot, an omission with a reason does not.
"""

import enum

import pytest


class Policy(enum.Enum):
    REPR_CONTENT = "repr"
    STR_CONTENT = "str"


class _Liar:
    pass


def _helper():
    pass


@pytest.mark.parametrize("n", [1, 2, 3])
def test_scalar_ids(n):
    assert n != 3


@pytest.mark.parametrize("s", ["plain", "(SELECT 1)", "a b", ""])
def test_string_ids_keep_printable_ascii(s):
    assert isinstance(s, str)


@pytest.mark.parametrize("p", list(Policy))
def test_enum_ids(p):
    assert p in Policy


@pytest.mark.parametrize("obj", [_Liar, _helper])
def test_named_objects_id_by_name(obj):
    assert obj is not None


@pytest.mark.parametrize("size", [1, 2], ids=["small", "large"])
def test_explicit_ids_win(size):
    assert size > 0


@pytest.mark.parametrize("v", [pytest.param(9, id="nine")])
def test_param_id_wins(v):
    assert v == 9
