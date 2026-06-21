"""N5 proof — riptide's native builtin resources (ROADMAP-v2 B1) driven through the REAL engine shim
(`engine/py-shim/shim.py`). **No pytest.** Proves, decisively:

  • type-DI for builtins: a param `mp: MonkeyPatch` / `p: TmpPath` / `cap: Capsys` wires to the
    auto-registered builtin provider BY TYPE — the param name need not match the provider name;
  • teardown isolation (the whole point of monkeypatch/tmp_path): a mutation made in one test is
    fully reversed before the next, and a tmp dir is removed after its test. Run in `no_fork=True`
    so teardown effects are observable in this one process (under fork each child is a fresh COW copy,
    which would mask — not prove — the undo);
  • capture: `capsys` (sys-level) and `capfd` (fd-level) each return the right `.readouterr()`.

Run:  python3 proof_n5_builtins.py
"""
from __future__ import annotations

import os
import sys
import tempfile
import textwrap

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)  # the `riptide` package (so the shim can `import riptide.builtins`)
sys.path.insert(0, os.path.join(_HERE, os.pardir, "py-shim"))  # `shim`

import shim  # noqa: E402

CORPUS = textwrap.dedent(
    '''
    import os
    import pathlib
    import sys

    from riptide.builtins import Capfd, Capsys, MonkeyPatch, TmpPath

    MARKER = "original"          # module attr a test patches, to prove setattr undo
    LEAK = {}                    # carries the tmp dir path to the next test, to prove cleanup

    def test_mp_mutates(mp: MonkeyPatch):          # `mp` ≠ provider name "monkeypatch" → BY TYPE
        mp.setenv("RIPTIDE_B1", "yes")
        mp.setattr(sys.modules[__name__], "MARKER", "patched")
        assert os.environ["RIPTIDE_B1"] == "yes" and MARKER == "patched"

    def test_mp_restored():                        # teardown of the prior test must have undone both
        assert "RIPTIDE_B1" not in os.environ
        assert MARKER == "original"

    def test_tmp_path_create(p: TmpPath):          # `p` ≠ "tmp_path" → BY TYPE; and it IS a real Path
        assert isinstance(p, pathlib.Path)
        f = p / "data.txt"
        f.write_text("hi")
        LEAK["dir"] = str(p)
        assert f.read_text() == "hi"

    def test_tmp_path_cleaned():                   # the dir from the prior test was removed at teardown
        d = LEAK.get("dir")
        assert d is not None and not os.path.exists(d)

    def test_capsys(cap: Capsys):
        print("captured-line")
        assert cap.readouterr().out == "captured-line\\n"

    def test_capfd(cap: Capfd):
        os.write(1, b"fd-level-write\\n")          # raw fd write — only capfd sees this
        assert "fd-level-write" in cap.readouterr().out
    '''
)


def main() -> int:
    print("=== N5 proof: native builtins through the real shim (type-DI + teardown, NO pytest) ===\n")
    with tempfile.TemporaryDirectory() as root:
        with open(os.path.join(root, "test_builtins.py"), "w") as f:
            f.write(CORPUS)

        sys.path.insert(0, root)
        shim._preimport(root)
        reg = shim._discover(root)

        builtin_names = sorted(n for n in reg.by_name if n in {"monkeypatch", "tmp_path", "capsys", "capfd"})
        print(f"[discovery] builtin providers auto-registered : {builtin_names}")
        by_type = {t.__name__: ns for t, ns in reg.by_type.items()}
        print(f"[discovery] builtin types indexed             : "
              f"{{ {', '.join(f'{t}->{by_type[t]}' for t in sorted(by_type) if t in {'MonkeyPatch','TmpPath','Capsys','Capfd'})} }}")
        registered_ok = builtin_names == ["capfd", "capsys", "monkeypatch", "tmp_path"]

        engine = shim.Engine(reg, no_fork=True)
        order = [
            ("test_mp_mutates", "passed"),
            ("test_mp_restored", "passed"),
            ("test_tmp_path_create", "passed"),
            ("test_tmp_path_cleaned", "passed"),
            ("test_capsys", "passed"),
            ("test_capfd", "passed"),
        ]
        results = {}
        print("\n[run]")
        for name, want in order:
            res = engine.run(f"test_builtins.py::{name}", "pytest_func", 5000)
            results[name] = res["outcome"]
            mark = "ok" if res["outcome"] == want else f"!! expected {want}"
            detail = f"  ({res['detail'].strip().splitlines()[-1]})" if res["detail"] else ""
            print(f"    {name:<24} {res['outcome']:<8} {mark}{detail}")
        engine.teardown_all()

        go = registered_ok and all(results[n] == w for n, w in order)
        print(f"\n=== VERDICT: {'GO — builtins resolve BY TYPE and tear down correctly through the real shim' if go else 'NO-GO'} ===")
        return 0 if go else 1


if __name__ == "__main__":
    raise SystemExit(main())
