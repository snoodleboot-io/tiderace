"""What discovery produced (TID-124, step 2): the registry, and everything the walk learned on the
way to it — the conftests and the directory each governs, the options and ini values they
declared, the skips the collection hooks decided, the directories a conftest skipped or broke —
as one [`Discovery`] the engine holds.

Eight module globals used to carry this (`_CLI_OPTIONS`, `_INI_DECLARED`, `_MARKER_SKIPS`,
`_ANCESTOR_CONFTESTS`, `_CONFTEST_SCOPES`, `_DIR_SKIPS`, `_DIR_ERRORS`, `_HOOK_MARKS`), two of
them declared after their first use, filled by `_discover` and its helpers and read back by the
gate, `request.config` and the `pytest_generate_tests` driver.
"""
from __future__ import annotations

import os
from dataclasses import dataclass, field
from typing import Any

from .safe import safe_getattr


def dir_mark(marks: dict, rel_path: str) -> str | None:
    """The mark (a skip reason, a conftest's import failure) covering `rel_path` — a suite-relative
    file or directory — from the nearest ancestor directory that carries one; `""` covers all."""
    if not marks:
        return None
    if "" in marks:
        return marks[""]
    parts = rel_path.split("/")
    for depth in range(len(parts), 0, -1):
        mark = marks.get("/".join(parts[:depth]))
        if mark is not None:
            return mark
    return None


@dataclass
class Discovery:
    """What one discovery produced. `registry` is the fixture registry it built; the rest is what
    the walk recorded, keyed the way the gate asks for it."""

    registry: Any  # the `Registry` — every fixture and provider, by name and by type
    # The conftests above the run root, `(module, location)` shallowest first (TID-19). Executed
    # once: a conftest's whole job is side effects, and running it twice would apply them twice.
    # `serve()` imports them before `_preimport`; `_discover` finds them here.
    ancestors: list | None = None
    # Every conftest imported, with the directory it governs: `""`/`"."` for the run root, a
    # `..`-relative location for an ancestor (which governs everything), else a root-relative
    # directory (TID-85).
    conftest_scopes: list = field(default_factory=list)
    # Command-line options conftests declared via `pytest_addoption`, as `dest -> default` (TID-14).
    # Only defaults: tiderace has no way to *pass* a custom flag yet (TID-17), so a declared option
    # always reads as its default — which is what an opt-in guard like
    # `if not request.config.getoption("--real"): pytest.skip(...)` needs to resolve correctly.
    cli_options: dict = field(default_factory=dict)
    # `parser.addini(name, help, type, default)` declarations, as `name -> (type, default)` (TID-87).
    # A value the project's config sets wins over the declared default; `getini` of a name nobody
    # declared is `None`.
    ini_declared: dict = field(default_factory=dict)
    # Node ids a collection hook (or a direct `@pytest.mark.skip`) decided to skip, as
    # `node_id -> reason` (TID-20). Computed once, consulted per node by the gate.
    marker_skips: dict = field(default_factory=dict)
    # Suite-relative dir (`""` = everything) -> why its conftest skipped it (TID-48).
    dir_skips: dict = field(default_factory=dict)
    # Suite-relative dir -> the traceback of its conftest's failed import (TID-72). pytest stops
    # at collection with one error and runs nothing; every test under that conftest is reported
    # with the conftest's own traceback, which is the same verdict per test.
    dir_errors: dict = field(default_factory=dict)
    # Node id -> the parametrize marks its `pytest_generate_tests` hooks produced (TID-85), filled
    # as nodes are planned: hooks are deterministic and `_cases` / `_indirect` both ask.
    hook_marks: dict = field(default_factory=dict)

    def dir_skip(self, rel_path: str) -> str | None:
        """The skip reason covering `rel_path`, if a conftest skipped it."""
        return dir_mark(self.dir_skips, rel_path)

    def dir_error(self, rel_path: str) -> str | None:
        """The conftest import failure covering `rel_path`, if one of its conftests did not import."""
        return dir_mark(self.dir_errors, rel_path)

    @property
    def generate_tests_hooks(self) -> bool:
        """Whether any conftest declares `pytest_generate_tests` — a suite without the hook pays a
        dictionary lookup per node, nothing more."""
        return any(safe_getattr(m, "pytest_generate_tests", None) is not None
                   for _, m in self.conftest_scopes)

    def conftests_governing(self, module_key: str) -> list:
        """The conftest modules whose directory holds `module_key`, deepest first — pytest's
        calling order for their hooks (a later-registered plugin is called first)."""
        module_dir = module_key.rsplit("/", 1)[0] if "/" in module_key else ""
        governing = []
        for location, module in self.conftest_scopes:
            loc = "" if location in (".", "") else location.replace(os.sep, "/")
            if loc.startswith(".."):
                depth = -1  # an ancestor: governs everything, called after every in-tree conftest
            elif loc == "" or module_dir == loc or module_dir.startswith(loc + "/"):
                depth = loc.count("/") + 1 if loc else 0
            else:
                continue
            governing.append((depth, module))
        governing.sort(key=lambda d: -d[0])
        return [m for _, m in governing]
