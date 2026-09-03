"""Seyd — hyper-low-latency P2P streaming for robots, as a Python package.

A thin wrapper over `libseyd` (ADR 0004): all protocol logic lives in the Rust
core, so this package and `seydd` and the C++/ROS 2 wrappers cannot drift.

    from seyd import Agent, ChannelKind

    with Agent("my-robot", "wss://signal.seyd.io/ws") as agent:
        video = agent.add_channel(ChannelKind.VIDEO, "main", fps=25)
        agent.on_command = lambda channel, payload: print(payload)
        agent.start()
        ...
"""

from ._ffi import HEADER_ABI_VERSION as ABI_VERSION, SeydLibraryNotFound
from .agent import (
    Agent,
    Channel,
    ChannelKind,
    Counters,
    Role,
    SeydError,
    SignalState,
)

__all__ = [
    "ABI_VERSION",
    "Agent",
    "Channel",
    "ChannelKind",
    "Counters",
    "Role",
    "SeydError",
    "SeydLibraryNotFound",
    "SignalState",
    "__version__",
]

__version__ = "0.1.0"
