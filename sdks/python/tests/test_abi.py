# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""The Python twin of sdks/c/examples/abi-smoke.c.

Exercises the wrapper against a real `libseyd` without touching the network:
the header matches the library, channel numbering is what the pilot will see,
lifecycle states are enforced, and failures raise rather than crash.

    tools/.venv/bin/python3 -m pytest sdks/python/tests
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import seyd  # noqa: E402
from seyd import Agent, ChannelKind, SeydError  # noqa: E402


def make_agent(**kw) -> Agent:
    return Agent(
        "abi-smoke",
        "ws://127.0.0.1:1/ws",
        credential_path="/dev/null",
        qos_profile="latency",
        **kw,
    )


def test_library_matches_header():
    assert seyd.ABI_VERSION == 1


def test_unknown_qos_profile_is_refused_before_any_network_work():
    with pytest.raises(SeydError) as e:
        Agent("abi-smoke", "ws://127.0.0.1:1/ws", qos_profile="no-such-profile")
    assert "no-such-profile" in str(e.value)


def test_channels_are_numbered_from_one_in_order():
    with make_agent() as agent:
        video = agent.add_channel(ChannelKind.VIDEO, "main", codec="avc1.42001f", fps=25)
        sensor = agent.add_channel(ChannelKind.SENSOR, "telemetry")
        command = agent.add_channel(ChannelKind.COMMAND, "ptz")
        assert (video.id, sensor.id, command.id) == (1, 2, 3)
        assert agent.channel("telemetry") is sensor
        assert [c.name for c in agent.channels] == ["main", "telemetry", "ptz"]
        # A channel is usable directly as a push target.
        assert int(video) == 1


def test_duplicate_channel_name_is_refused():
    with make_agent() as agent:
        agent.add_channel(ChannelKind.SENSOR, "telemetry")
        with pytest.raises(SeydError):
            agent.add_channel(ChannelKind.SENSOR, "telemetry")


def test_video_channel_defaults_to_h264_baseline():
    with make_agent() as agent:
        assert agent.add_channel(ChannelKind.VIDEO, "main").codec == "avc1.42001f"
        assert agent.add_channel(ChannelKind.SENSOR, "t").codec == "json"


def test_pushing_before_start_is_an_error_not_a_crash():
    with make_agent() as agent:
        video = agent.add_channel(ChannelKind.VIDEO, "main")
        sensor = agent.add_channel(ChannelKind.SENSOR, "telemetry")
        with pytest.raises(SeydError):
            agent.push_frame(video, b"\x00", keyframe=True, capture_ts_us=0)
        with pytest.raises(SeydError):
            agent.push_message(sensor, {"seq": 1})


def test_stop_and_close_are_idempotent():
    agent = make_agent()
    agent.add_channel(ChannelKind.SENSOR, "telemetry")
    agent.stop()
    agent.close()
    agent.close()


def test_handlers_may_be_assigned_after_construction():
    # The trampolines are installed at create time and dispatch to whatever is
    # set when an event fires, so this must not raise.
    with make_agent() as agent:
        agent.on_command = lambda channel, payload: None
        agent.on_session_ended = lambda session_id, reason: None
        assert agent.on_command is not None
