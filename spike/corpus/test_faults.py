"""Phase-1 spike corpus — fault tests (C3): the forked child dies; the Wellspring must survive
and report Outcome::Error. Excluded from the differential corpus (these would take down a
shared-process runner)."""
import os
import time


def test_hard_crash():
    # Simulate a segfault-like hard exit in the child (bypasses normal result reporting).
    os._exit(139)


def test_hang_times_out():
    # Never returns — the orchestrator's deadline must kill the child and report Error.
    time.sleep(3600)
