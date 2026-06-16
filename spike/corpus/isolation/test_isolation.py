"""Phase-1 spike corpus — the fork-isolation proof (C2).

A module-level mutable global is mutated by the first test. Under fork-from-warm isolation,
each test runs in a pristine COW child, so the second test sees the ORIGINAL value (0) and
passes. Under a shared-process runner (e.g. stock pytest in one process, in definition order)
the second test would see 1 and FAIL — that divergence is exactly the isolation win, so these
tests are NOT part of the differential corpus.
"""

counter = 0


def test_a_mutates_global():
    global counter
    counter += 1
    assert counter == 1


def test_b_sees_pristine_state():
    # Passes only if this test ran in its own pristine fork (counter still 0).
    assert counter == 0
