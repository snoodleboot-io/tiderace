"""The little that a test legitimately needs from the runner's own configuration."""
from __future__ import annotations

import os
from typing import Any


class RunConfig:
    """What `pytestconfig` gives a test, as much as is meaningful without pytest.

    Tests reach for this to read an option or find the project root — `config.getoption("--real")`,
    `config.rootpath`. Options come from the project's own `addopts` and from `pytest_addoption`
    defaults a conftest declared (TID-14), which is what tiderace already knows; anything it does not
    know returns the caller's `default` rather than inventing a value.
    """

    __slots__ = ("_options", "_rootdir")

    def __init__(self, options: dict | None = None, rootdir: str | None = None) -> None:
        self._options = dict(options or {})
        self._rootdir = rootdir or os.getcwd()

    def getoption(self, name: str, default: Any = None, skip: bool = False) -> Any:
        """An option's value by flag (`--real`) or dest (`real`), else `default`."""
        for key in (name, name.lstrip("-").replace("-", "_")):
            if key in self._options:
                return self._options[key]
        return default

    def getini(self, name: str) -> Any:
        """ini values are not modelled; returns `None` rather than guessing."""
        return None

    @property
    def rootpath(self):
        from pathlib import Path

        return Path(self._rootdir)

    @property
    def rootdir(self):
        return self.rootpath

    @property
    def inipath(self):
        return None

    def __repr__(self) -> str:
        return f"RunConfig(rootdir={self._rootdir!r}, options={sorted(self._options)})"
