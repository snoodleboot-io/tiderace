"""Runnable proof of tiderace's native type-driven authoring (ADR-E012, step N1). **No pytest.**

Demonstrates, with a tiny self-contained runner (the real runner is the shim's job later):
  • type-DI: a test parameter `db: Db` wires to the provider returning `Db` — by TYPE, not name;
  • provider→provider DI: a provider depends on another provider, also by type;
  • yield teardown: yield-style providers tear down in reverse at scope exit;
  • parametrization via @tiderace.cases (one passing, one failing — failure captured);
  • the native error taxonomy: untyped / unprovided / ambiguous all raise TideraceResolutionError.

Run:  python3 proof_type_di.py
"""
from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))  # find the local `tiderace` package

import tiderace
from tiderace import ProviderSpec  # for the ambiguity demo

_LOG: list[str] = []  # records provider lifecycle so we can assert ordering


# --------------------------------------------------------------------- user code under test
class Db:
    def __init__(self):
        self.rows: list[str] = []
        self.open = True

    @classmethod
    def connect(cls) -> "Db":
        _LOG.append("db.connect")
        return cls()

    def add(self, x: str) -> None:
        self.rows.append(x)

    def count(self) -> int:
        return len(self.rows)

    def close(self) -> None:
        self.open = False
        _LOG.append("db.close")


class User:
    def __init__(self, db: Db, name: str):
        self.db = db
        self.name = name


@tiderace.provides(scope="module")
def db() -> Db:                       # plain `-> Db` on a yield provider (the migrated form)
    conn = Db.connect()
    yield conn
    conn.close()


@tiderace.provides
def user(db: Db) -> User:             # a provider that itself depends on `Db` BY TYPE
    _LOG.append("user.build")
    return User(db, "ada")


def test_insert(db: Db):              # wired to the `db` provider by the type `Db`
    db.add("ada")
    assert db.count() == 1


def test_user_is_wired_to_db(user: User):
    assert isinstance(user.db, Db)
    assert user.name == "ada"


@tiderace.cases([(2, 3, 5), (2, 2, 5)])   # the second row is wrong on purpose
def test_add(a, b, exp):
    assert a + b == exp


# --------------------------------------------------------------------- the tiny runner
def _collect_providers(mod) -> dict:
    providers = {}
    for obj in vars(mod).values():
        spec = getattr(obj, "__tiderace_provider__", None)
        if spec is not None:
            providers[spec.name] = (obj, spec)
    return providers


def _instantiate(name, providers, index, teardown):
    fn, spec = providers[name]
    deps = tiderace.resolve_params(fn, index)  # the provider's OWN params, wired by type
    kwargs = {p: _instantiate(pn, providers, index, teardown) for p, pn in deps.items()}
    if spec.is_yield:
        gen = fn(**kwargs)
        value = next(gen)
        teardown.append(gen)
        return value
    return fn(**kwargs)


def _run(mod) -> list[tuple]:
    providers = _collect_providers(mod)
    index = tiderace.build_type_index(spec for _, spec in providers.values())
    results: list[tuple] = []
    for name, obj in list(vars(mod).items()):
        if not (name.startswith("test_") and callable(obj)):
            continue
        for case in getattr(obj, "__tiderace_cases__", None) or [None]:
            teardown: list = []
            label = f"{name}[{case.id}]" if case else name
            try:
                if case is not None:
                    obj(*case.values)
                else:
                    deps = tiderace.resolve_params(obj, index)
                    kwargs = {p: _instantiate(pn, providers, index, teardown) for p, pn in deps.items()}
                    obj(**kwargs)
                results.append((label, "passed", ""))
            except AssertionError as exc:
                results.append((label, "failed", str(exc) or "assert"))
            finally:
                for gen in reversed(teardown):
                    try:
                        next(gen)
                    except StopIteration:
                        pass
    return results


def _expect_resolution_error(label, thunk) -> bool:
    try:
        thunk()
    except tiderace.TideraceResolutionError as exc:
        print(f"    {label}: TideraceResolutionError ✓  — {exc}")
        return True
    print(f"    {label}: NO ERROR (BAD)")
    return False


def main() -> int:
    print("=== tiderace native type-driven authoring — proof (NO pytest) ===\n")

    results = _run(sys.modules[__name__])
    print("[run] tests (wired by type, executed by the tiny runner):")
    for label, outcome, detail in results:
        suffix = f"  ({detail})" if detail else ""
        print(f"    {label:<26} {outcome}{suffix}")

    print(f"\n[lifecycle] provider order: {_LOG}")
    teardown_ok = _LOG.count("db.connect") == _LOG.count("db.close") and "db.close" in _LOG

    print("\n[errors] native taxonomy (type-DI failures are hard errors, never silent):")
    index = tiderace.build_type_index(
        [ProviderSpec(provides=Db, scope="module", autouse=False, name=n, is_yield=True)
         for n in ("primary", "secondary")]
    )
    def _untyped(x):  # noqa: ANN001
        return x
    errs = [
        _expect_resolution_error("untyped param", lambda: tiderace.resolve_params(_untyped, {})),
        _expect_resolution_error("no provider for type", lambda: tiderace.resolve_params(test_user_is_wired_to_db, {})),
        _expect_resolution_error("ambiguous type", lambda: tiderace.resolve_params(test_insert, index)),
    ]

    by_label = {r[0]: r[1] for r in results}
    go = (
        by_label.get("test_insert") == "passed"
        and by_label.get("test_user_is_wired_to_db") == "passed"
        and by_label.get("test_add[2-3-5]") == "passed"
        and by_label.get("test_add[2-2-5]") == "failed"
        and teardown_ok
        and all(errs)
    )
    print(f"\n=== VERDICT: {'GO — type-DI, provider→provider, yield teardown, params, native errors all work' if go else 'NO-GO'} ===")
    return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
