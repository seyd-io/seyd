"""The Seyd robot agent, as Python.

A thin, Pythonic shape over the C ABI: no protocol logic lives here (ADR 0004
rule 1). Everything this module does is marshal arguments, keep C callback
trampolines alive, and turn status codes into exceptions.
"""

from __future__ import annotations

import enum
import json
from typing import Callable, Iterator, Mapping, Sequence

from ._ffi import ffi, lib, last_error, to_c

__all__ = [
    "Agent",
    "ChannelKind",
    "Role",
    "SignalState",
    "SeydError",
    "Counters",
]


class ChannelKind(enum.IntEnum):
    """What a channel carries. Video is FEC-protected and whole-frame-or-nothing."""

    VIDEO = 0
    SENSOR = 1
    COMMAND = 2


class Role(enum.IntEnum):
    """A driver may send commands; an observer's commands are dropped by the agent."""

    DRIVER = 0
    OBSERVER = 1


class SignalState(enum.IntEnum):
    CONNECTED = 0
    DISCONNECTED = 1
    #: Authentication was refused — almost always a configuration problem.
    DENIED = 2


class SeydError(RuntimeError):
    """A call into `libseyd` failed. `status` is the `seyd_status` code."""

    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status = status


def _check(status: int) -> None:
    if status != lib.SEYD_OK:
        raise SeydError(status, last_error() or f"seyd status {status}")


class Counters(dict):
    """Engine counters. A plain mapping, with attribute access for convenience."""

    def __getattr__(self, name: str):
        try:
            return self[name]
        except KeyError as e:
            raise AttributeError(name) from e


_COUNTER_FIELDS = (
    "frames_in",
    "frames_sent",
    "frames_dropped_backlog",
    "frames_skipped_stale",
    "keyframes_requested",
    "chunks_sent",
    "parity_sent",
    "bytes_sent",
)


class Channel:
    """A configured channel. Truthy id; usable directly as a `push_*` target."""

    __slots__ = ("id", "kind", "name", "codec", "fps")

    def __init__(self, id: int, kind: ChannelKind, name: str, codec: str, fps: int):
        self.id = id
        self.kind = kind
        self.name = name
        self.codec = codec
        self.fps = fps

    def __index__(self) -> int:
        return self.id

    def __repr__(self) -> str:
        return f"<Channel {self.id} {self.kind.name.lower()} {self.name!r}>"


class Agent:
    """A Seyd robot.

    Configure channels, register handlers, then :meth:`start`. Handlers run on
    one Seyd thread, in order, and **must not block** — a slow handler delays
    every later event. Do real work on your own thread or queue.

        with Agent("my-robot", "wss://signal.example/ws") as agent:
            video = agent.add_channel(ChannelKind.VIDEO, "main",
                                      codec="avc1.42001f", fps=25)
            agent.on_command = lambda ch, data: drive(data)
            agent.start()
            for au in encoder:
                agent.push_frame(video, au.bytes, keyframe=au.is_idr,
                                 capture_ts_us=au.pts_us)
    """

    def __init__(
        self,
        robot_id: str,
        signal_url: str,
        *,
        credential_path: str | None = None,
        quic_port: int = 0,
        ipv6: bool = True,
        port_mapping: bool = True,
        host_override: str | None = None,
        qos_profile: str = "balanced",
        max_sessions: int = 0,
    ):
        #: Called as ``(session_id, role, path_label)`` when a pilot is admitted.
        self.on_session_started: Callable[[str, Role, str], None] | None = None
        #: Called as ``(session_id, reason)``. **Park actuators here** — it fires
        #: whether the pilot said goodbye or simply vanished.
        self.on_session_ended: Callable[[str, str], None] | None = None
        #: Called as ``(channel_id, payload)`` for each driver command.
        self.on_command: Callable[[int, bytes], None] | None = None
        #: Called as ``(config)`` with Seyd's targets for the publisher — the
        #: ``video-config`` document of docs/protocol/seydd.md: ``maxBitrateKbps``,
        #: ``latencyBudgetMs``, ``maxGopMs``, ``preferIntraRefresh``,
        #: ``suggestedFps``, ``reason``. The one place Seyd talks to the encoder.
        #: Keyframes are on demand (ADR 0009): answer ``on_recovery_request`` and
        #: treat ``maxGopMs`` as a long ceiling, not a cadence.
        self.on_requested_config: Callable[[Mapping], None] | None = None
        #: Called as ``(channel_id, kind, reason)`` where kind is ``ltr``,
        #: ``intra_refresh`` or ``idr``.
        self.on_recovery_request: Callable[[int, str, str], None] | None = None
        #: Called as ``(state, detail)``; the robot is unreachable while down.
        self.on_signal_state: Callable[[SignalState, str | None], None] | None = None
        #: Called as ``(report)`` with the NAT report each time candidates are announced.
        self.on_nat_report: Callable[[Mapping], None] | None = None

        self._channels: list[Channel] = []
        self._started = False
        self._handle = ffi.new("seyd_agent **")
        # cffi callbacks must outlive the C side that holds them.
        self._trampolines = self._build_trampolines()
        self._keepalive = [
            to_c(robot_id),
            to_c(signal_url),
            to_c(credential_path),
            to_c(host_override),
            to_c(qos_profile),
        ]
        cfg = ffi.new(
            "seyd_config *",
            {
                "robot_id": self._keepalive[0],
                "signal_url": self._keepalive[1],
                "credential_path": self._keepalive[2],
                "quic_port": quic_port,
                "ipv6": ipv6,
                "port_mapping": port_mapping,
                "host_override": self._keepalive[3],
                "qos_profile": self._keepalive[4],
                "max_sessions": max_sessions,
            },
        )
        _check(lib.seyd_agent_create(cfg, self._trampolines, self._handle))
        self._agent = self._handle[0]

    # ── configuration ────────────────────────────────────────────────────

    def add_channel(
        self,
        kind: ChannelKind,
        name: str,
        *,
        codec: str | None = None,
        fps: int = 0,
    ) -> Channel:
        """Add a channel. Only valid before :meth:`start`.

        Channels are numbered from 1 in the order they are added, and that
        numbering is what the pilot sees.
        """
        codec = codec or ("avc1.42001f" if kind == ChannelKind.VIDEO else "json")
        name_c, codec_c = to_c(name), to_c(codec)
        cfg = ffi.new(
            "seyd_channel_config *",
            {"kind": int(kind), "name": name_c, "codec": codec_c, "fps": fps},
        )
        out = ffi.new("uint8_t *")
        _check(lib.seyd_channel_add(self._agent, cfg, out))
        channel = Channel(out[0], ChannelKind(kind), name, codec, fps)
        self._channels.append(channel)
        return channel

    @property
    def channels(self) -> Sequence[Channel]:
        return tuple(self._channels)

    def channel(self, name: str) -> Channel:
        """The channel added under `name`."""
        for c in self._channels:
            if c.name == name:
                return c
        raise KeyError(name)

    # ── lifecycle ────────────────────────────────────────────────────────

    def start(self) -> None:
        """Bind, discover, announce and start serving.

        Blocks while candidates are gathered (a second or two). Handlers begin
        firing once this returns.
        """
        _check(lib.seyd_agent_start(self._agent))
        self._started = True

    def stop(self) -> None:
        """Stop serving. Blocks until no further handler can run. Idempotent."""
        if self._agent is not None:
            _check(lib.seyd_agent_stop(self._agent))
        self._started = False

    def close(self) -> None:
        """Stop and release the agent. The object is unusable afterwards."""
        if getattr(self, "_agent", None) is not None:
            lib.seyd_agent_destroy(self._agent)
            self._agent = None
            self._started = False

    def __enter__(self) -> "Agent":
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def __del__(self):
        try:
            self.close()
        except Exception:  # interpreter teardown; nothing useful to do
            pass

    # ── media ────────────────────────────────────────────────────────────

    def push_frame(
        self,
        channel: Channel | int,
        data: bytes,
        *,
        keyframe: bool,
        capture_ts_us: int,
    ) -> None:
        """Hand one encoded access unit to a video channel.

        Never blocks. A frame arriving while the previous one is still going
        out is dropped by admission control — keyframes never are — and counted
        in ``frames_dropped_backlog``. `capture_ts_us` is the source's capture
        clock; the pilot paces presentation on it (ADR 0005), so pass the
        encoder's timestamp rather than "now" whenever you have it.
        """
        _check(
            lib.seyd_push_frame(
                self._agent,
                int(channel),
                ffi.from_buffer(data),
                len(data),
                keyframe,
                capture_ts_us,
            )
        )

    def push_message(self, channel: Channel | int, data: bytes | str | Mapping) -> None:
        """Send a message on a sensor channel. Never blocks.

        `dict` is encoded as JSON and `str` as UTF-8, since that is what almost
        every sensor channel carries.
        """
        if isinstance(data, Mapping):
            data = json.dumps(data, separators=(",", ":"))
        if isinstance(data, str):
            data = data.encode("utf-8")
        _check(
            lib.seyd_push_message(
                self._agent, int(channel), ffi.from_buffer(data), len(data)
            )
        )

    # ── runtime state ────────────────────────────────────────────────────

    def set_qos_profile(self, name: str) -> None:
        """Switch the QoS ceiling (`latency`, `balanced`, `quality`)."""
        _check(lib.seyd_set_qos_profile(self._agent, to_c(name)))

    def set_status(self, status: Mapping | None) -> None:
        """Publish free-form robot status to the console. `None` clears it."""
        payload = None if status is None else json.dumps(status)
        _check(lib.seyd_set_status_json(self._agent, to_c(payload)))

    @property
    def session_count(self) -> int:
        out = ffi.new("uint32_t *")
        _check(lib.seyd_session_count(self._agent, out))
        return out[0]

    @property
    def counters(self) -> Counters:
        out = ffi.new("seyd_counters_t *")
        _check(lib.seyd_counters(self._agent, out))
        return Counters({f: getattr(out, f) for f in _COUNTER_FIELDS})

    # ── callback plumbing ────────────────────────────────────────────────

    def _build_trampolines(self):
        """One C function per event, dispatching to whatever handler is set at
        the time it fires — so handlers can be assigned after construction.

        An exception in a handler is reported by cffi on stderr and swallowed;
        it cannot unwind into Rust or C.
        """
        keep = []

        def cb(signature: str):
            def register(fn):
                trampoline = ffi.callback(signature, fn)
                keep.append(trampoline)
                return trampoline

            return register

        @cb("void(void *, char *, seyd_role, char *)")
        def session_started(user, session_id, role, path_label):
            if self.on_session_started:
                self.on_session_started(
                    _str(session_id), Role(role), _str(path_label)
                )

        @cb("void(void *, char *, char *)")
        def session_ended(user, session_id, reason):
            if self.on_session_ended:
                self.on_session_ended(_str(session_id), _str(reason))

        @cb("void(void *, uint8_t, uint8_t *, size_t)")
        def command(user, channel, data, length):
            if self.on_command:
                self.on_command(channel, bytes(ffi.buffer(data, length)))

        @cb("void(void *, char *)")
        def requested_config(user, payload):
            if self.on_requested_config:
                self.on_requested_config(json.loads(_str(payload)))

        @cb("void(void *, uint8_t, char *, char *)")
        def recovery_request(user, channel, kind, reason):
            if self.on_recovery_request:
                self.on_recovery_request(channel, _str(kind), _str(reason))

        @cb("void(void *, seyd_signal_state, char *)")
        def signal_state(user, state, detail):
            if self.on_signal_state:
                self.on_signal_state(SignalState(state), _str(detail))

        @cb("void(void *, char *)")
        def nat_report(user, payload):
            if self.on_nat_report:
                self.on_nat_report(json.loads(_str(payload)))

        self._callback_keepalive = keep
        return ffi.new(
            "seyd_callbacks *",
            {
                "user": ffi.NULL,
                "on_session_started": session_started,
                "on_session_ended": session_ended,
                "on_command": command,
                "on_requested_config": requested_config,
                "on_recovery_request": recovery_request,
                "on_signal_state": signal_state,
                "on_nat_report": nat_report,
            },
        )


def _str(p) -> str | None:
    """A C string as `str`; `None` for NULL."""
    if p == ffi.NULL:
        return None
    return ffi.string(p).decode("utf-8", "replace")
