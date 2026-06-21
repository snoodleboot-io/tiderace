"""A deliberately mixed pytest suite — input for `riptide migrate` (proof of N4). Some constructs map
cleanly to type-DI; others can't and must be reported. NOT executed by the migrator (pure ast)."""
import pytest


class Db:
    pass


@pytest.fixture(scope="module")
def db() -> Db:            # typed yield fixture → maps cleanly to @riptide.provides
    yield Db()


@pytest.fixture
def cache():               # UNTYPED → type can't be inferred (the #1 type-DI migration gap)
    return {}


@pytest.fixture(params=[1, 2])   # parametrized fixture → provider-level params not in riptide yet
def variant(request):
    return request.param


@pytest.mark.parametrize("a,b,exp", [(1, 2, 3), (2, 2, 4)])
def test_add(a, b, exp):
    assert a + b == exp


@pytest.mark.skipif(True, reason="env")
def test_db(db):           # db is typed → migrates to `db: Db`
    assert db is not None


def test_cache(cache):     # cache is untyped → flagged for manual annotation
    assert cache == {}


@pytest.mark.usefixtures("db")
def test_uses():           # string fixture name, no type → flagged
    pass


def test_tmp(tmp_path):    # pytest builtin → no riptide equivalent yet → flagged
    assert tmp_path


@pytest.mark.slow
def test_tagged():         # arbitrary mark → @riptide.tag("slow")
    pass
