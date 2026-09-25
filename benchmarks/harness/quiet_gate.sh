#!/usr/bin/env bash
# Wait for the machine to stop being oversubscribed by other work, then run the given command.
#
#   benchmarks/harness/quiet_gate.sh 5 python benchmarks/harness/timing_rr.py pirn-core
#
# A wall-clock comparison at load 20 on 8 cores measures the contention, not the runners: an idle
# worker costs nothing when the OS has someone else to give its cycles to, so the very thing a
# better scheduler removes is invisible. Below the core count an idle worker is genuinely idle
# machine. Gate on that, and record the load with every sample anyway.
set -euo pipefail
MAX="${1:?max 1-minute load}"; shift
until awk -v m="$MAX" '{exit !($1 < m)}' /proc/loadavg; do sleep 60; done
echo "machine quiet enough at load $(cut -d' ' -f1 /proc/loadavg) — starting"
exec "$@"
