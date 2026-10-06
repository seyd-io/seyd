#!/usr/bin/env python3
# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""
User-space link shaper between a pilot on this machine and the robot's QUIC
port, for measuring what a rate-limited uplink does to the pipeline without
root (macOS `dnctl` needs it). Models a modem: the robot→pilot direction is a
rate-limited FIFO with a bounded queue and tail drop; both directions get a
fixed one-way delay.

How it gets into the path: it binds 127.0.0.1:PORT with SO_REUSEADDR, which
macOS allows next to seydd's wildcard 0.0.0.0:PORT bind, and the specific bind
wins loopback traffic. Run seydd with `host_override = "127.0.0.1"` so the
loopback address is the only candidate the pilot is offered, and point
UPSTREAM at the robot through its LAN address, which the wildcard bind still
serves.

    python3 tools/link-shaper.py --port 4433 --upstream 192.168.1.20:4433 \
        --kbps 4500 --queue-kb 200 --delay-ms 20

Prints per-flow forwarded/dropped counts and the peak queue every 5 s. Used
for the 2026-09-08 intra-refresh A/B in docs/latency-sources.md.
"""
import argparse, asyncio, collections, socket, time


class Shaper:
    """Rate-limited FIFO link with a bounded queue and a pipelined delay.

    Serialisation is modelled by a running `next_free` clock: a packet begins
    transmitting when the link is free and finishes len/rate later; it is
    delivered `delay` after that. Bytes count as queued until they finish
    transmitting. Nothing sleeps per packet — every event is a timer.
    """
    def __init__(self, kbps, queue_bytes, delay_s, send):
        self.rate = kbps * 1000 / 8.0   # bytes per second
        self.qcap = queue_bytes
        self.delay = delay_s
        self.send = send
        self.qbytes = 0
        self.dropped = 0
        self.forwarded = 0
        self.max_q = 0
        self.next_free = 0.0

    def push(self, data, addr):
        loop = asyncio.get_event_loop()
        now = loop.time()
        if self.next_free < now:
            self.next_free = now
        if self.qbytes + len(data) > self.qcap:
            self.dropped += 1
            return
        self.qbytes += len(data)
        self.max_q = max(self.max_q, self.qbytes)
        self.next_free += len(data) / self.rate
        done_at = self.next_free
        loop.call_at(done_at, self._serialised, len(data))
        loop.call_at(done_at + self.delay, self._deliver, data, addr)

    def _serialised(self, n):
        self.qbytes -= n

    def _deliver(self, data, addr):
        self.send(data, addr)
        self.forwarded += 1


class Proxy(asyncio.DatagramProtocol):
    """Pilot-facing socket. One upstream socket per pilot source address."""
    def __init__(self, upstream, shaper_factory, delay_s):
        self.upstream = upstream
        self.shaper_factory = shaper_factory
        self.delay = delay_s
        self.flows = {}

    def connection_made(self, transport):
        self.transport = transport

    def datagram_received(self, data, addr):
        flow = self.flows.get(addr)
        if flow is None:
            flow = self.flows[addr] = asyncio.ensure_future(self.open_flow(addr))
            flow.pending = [data]
            return
        if not flow.done():
            flow.pending.append(data)
            return
        up = flow.result()
        asyncio.get_event_loop().call_later(self.delay, up.transport.sendto, data, self.upstream)

    async def open_flow(self, addr):
        loop = asyncio.get_event_loop()
        shaper = self.shaper_factory(lambda d, a: self.transport.sendto(d, addr))
        _, up = await loop.create_datagram_endpoint(lambda: Upstream(shaper), remote_addr=self.upstream)
        me = self.flows[addr]
        for d in me.pending:
            loop.call_later(self.delay, up.transport.sendto, d, self.upstream)
        return up


class Upstream(asyncio.DatagramProtocol):
    def __init__(self, shaper):
        self.shaper = shaper

    def connection_made(self, transport):
        self.transport = transport

    def datagram_received(self, data, addr):
        self.shaper.push(data, None)


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--port', type=int, default=4434)
    ap.add_argument('--upstream', required=True)
    ap.add_argument('--kbps', type=float, default=4500)
    ap.add_argument('--queue-kb', type=float, default=200)
    ap.add_argument('--delay-ms', type=float, default=20)
    a = ap.parse_args()
    host, port = a.upstream.rsplit(':', 1)
    upstream = (host, int(port))
    shapers = []

    def factory(send):
        s = Shaper(a.kbps, int(a.queue_kb * 1024), a.delay_ms / 1000.0, send)
        shapers.append(s)
        return s

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(('127.0.0.1', a.port))
    loop = asyncio.get_event_loop()
    await loop.create_datagram_endpoint(lambda: Proxy(upstream, factory, a.delay_ms / 1000.0), sock=sock)
    print(f'shaping 127.0.0.1:{a.port} -> {a.upstream}: down {a.kbps:.0f} kbps, queue {a.queue_kb:.0f} KB, one-way delay {a.delay_ms:.0f} ms', flush=True)
    while True:
        await asyncio.sleep(5)
        for i, s in enumerate(shapers):
            print(f't={time.strftime("%H:%M:%S")} flow{i} fwd={s.forwarded} dropped={s.dropped} max_queue={s.max_q}B queued={s.qbytes}B', flush=True)
            s.max_q = s.qbytes


if __name__ == '__main__':
    asyncio.run(main())
