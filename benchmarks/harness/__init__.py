"""The real-suite benchmark harness as a package (TID-112).

Every pass is a module run from the repository root — `python -m benchmarks.harness.parity` —
and shares three things instead of copying them: the corpus table (`corpora`), the timed run,
the interleaving and the two command lines (`runs`), and where the JSON lands (`reports`).
The method is in this directory's README.
"""
