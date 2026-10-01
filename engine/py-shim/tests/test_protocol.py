"""The shim's one transport (TID-122), offline: frames over a socketpair, the serve loop, and the
children `run_child` forks — a reply in time, a child that sleeps past the deadline, one that
exits without a frame, one that writes half a frame and stops."""
from __future__ import annotations

import os
import socket
import sys
import time

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from tiderace_shim import protocol  # noqa: E402

pytestmark = pytest.mark.skipif(not hasattr(os, "fork"), reason="the shim's children are forks")


def test_frames_round_trip_and_eof_is_none():
    a, b = socket.socketpair()
    protocol.write_frame(a.fileno(), {"node_id": "t.py::a", "detail": "é"})
    assert protocol.read_frame(b.fileno()) == {"node_id": "t.py::a", "detail": "é"}
    a.close()
    assert protocol.read_frame(b.fileno()) is None


def test_the_serve_loop_answers_until_eof_and_a_none_reply_is_silence():
    ours, theirs = socket.socketpair()
    t = protocol.Transport.over(theirs)
    seen = []

    def handler(req):
        seen.append(req)
        return None if req.get("quiet") else {"echo": req["n"]}

    def server() -> int:
        ours.close()  # the child's copy of the other end: without this its own EOF never comes
        t.serve(handler)
        return 0

    pid = protocol.spawn(server)
    client = protocol.Transport.over(ours)
    client.send({"n": 1})
    assert client.recv() == {"echo": 1}
    client.send({"n": 2, "quiet": True})
    client.send({"n": 3})
    assert client.recv() == {"echo": 3}  # the quiet request got no frame
    ours.close()
    status = protocol.reap(pid)
    assert protocol.exit_code(status) == 0


def test_a_reply_within_the_deadline():
    got = protocol.run_child(lambda: {"outcome": "passed", "detail": ""}, 5.0)
    assert got.reply == {"outcome": "passed", "detail": ""}
    assert not got.timed_out and got.exit_code == 0 and got.error is None
    assert got.received == 4 + len(b'{"outcome": "passed", "detail": ""}')


def test_a_child_that_sleeps_past_the_deadline_is_killed():
    started = time.monotonic()
    got = protocol.run_child(lambda: time.sleep(30) or {}, 0.3)
    assert time.monotonic() - started < 5
    assert got.timed_out and got.reply is None and got.received == 0
    assert got.signaled and got.exit_text == "killed by signal 9"


def test_a_child_that_exits_without_a_frame():
    def body():
        os._exit(3)

    got = protocol.run_child(body, 5.0)
    assert got.reply is None and not got.timed_out and got.exit_code == 3 and got.exit_text == "exited 3"
    assert got.received == 0


def test_an_unserialisable_reply_exits_unreportable():
    got = protocol.run_child(lambda: {"bad": object()}, 5.0)
    assert got.reply is None and got.exit_code == protocol.EXIT_UNREPORTABLE
    assert "could not serialise" in got.exit_text


def test_half_a_frame_then_silence_is_a_timeout_with_bytes_received():
    read_fd, write_fd = os.pipe()
    os.write(write_fd, b"\x10\x00\x00\x00abc")  # a 16-byte frame of which 3 arrived
    payload, timed_out = protocol.read_frame_by(read_fd, time.monotonic() + 0.2)
    assert payload is None and timed_out


def test_end_child_gives_grace_then_kills():
    sleeper = protocol.spawn(lambda: time.sleep(30) or 0)
    started = time.monotonic()
    status = protocol.end_child(sleeper, 0.2)
    assert time.monotonic() - started < 5
    assert protocol.exit_text(status) == "killed by signal 9"
    prompt = protocol.spawn(lambda: 0)
    assert protocol.exit_code(protocol.end_child(prompt, 5.0)) == 0
