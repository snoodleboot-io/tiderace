"""The little that a test legitimately needs from the runner's own configuration."""
from __future__ import annotations

import os
from typing import Any


class NullPluginManager:
    """The plugin manager a runner with no plugins can honestly offer.

    Suites reach for it to switch pytest's own machinery off for a test — flask's logging tests do
    `pytestconfig.pluginmanager.unregister(name="logging-plugin")` so that pytest's log capture stops
    intercepting the stream they are about to assert on. Under tiderace that plugin was never
    installed, so the request is already satisfied and the honest answer is "there is nothing here":
    unregistering returns `None`, and nothing is ever found registered.

    It exists because the alternative is an `AttributeError` during fixture setup, which fails six
    flask tests for a reason that has nothing to do with what they test.
    """

    __slots__ = ()

    def unregister(self, plugin=None, name=None):
        """Nothing was registered, so nothing is removed. Returns `None`, as pytest does for an
        unknown name."""
        return None

    def register(self, plugin, name=None):
        return None

    def get_plugin(self, name):
        return None

    def has_plugin(self, name) -> bool:
        return False

    def list_name_plugin(self) -> list:
        return []

    def is_registered(self, plugin) -> bool:
        return False

    def __repr__(self) -> str:
        return "<NullPluginManager: tiderace runs no pytest plugins>"


class RunConfig:
    """What `pytestconfig` gives a test, as much as is meaningful without pytest.

    Tests reach for this to read an option or find the project root — `config.getoption("--real")`,
    `config.rootpath`. Options come from the project's own `addopts` and from `pytest_addoption`
    defaults a conftest declared (TID-14), which is what tiderace already knows; anything it does not
    know returns the caller's `default` rather than inventing a value.
    """

    __slots__ = ("_options", "_rootdir", "pluginmanager")

    def __init__(self, options: dict | None = None, rootdir: str | None = None) -> None:
        self._options = dict(options or {})
        self._rootdir = rootdir or os.getcwd()
        self.pluginmanager = NullPluginManager()

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
