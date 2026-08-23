"""
STUN client (RFC 5389) + local candidate discovery.

The agent binds its own UDP socket and runs STUN on *that* socket before handing
it to aioquic. This matters: an earlier version bound a throwaway socket, closed
it, and let aioquic rebind the same port, which meant the reflexive address we
advertised was only correct if the NAT happened to hand out the same external
port on the rebind. On port-preserving home routers it usually did; on strict or
load-balancing NATs it silently did not, and the pilot got an address nobody was
listening on. Same socket, never closed, never rebound — the mapping we discover
is the mapping aioquic uses.

Two STUN servers on different operators are queried, which lets us distinguish
a cone NAT (both report the same external ip:port) from a symmetric NAT (each
reports a different port, because the mapping is chosen per destination). A
symmetric NAT makes the reflexive candidate worthless: the port a third party
would have to send to is not the one either STUN server saw.
"""

import asyncio
import ipaddress
import logging
import secrets
import socket
import struct
from dataclasses import dataclass, field
from typing import Optional

log = logging.getLogger(__name__)

# Two operators, so the two queries land on genuinely different server IPs.
# Anycast within one provider could otherwise hide a symmetric NAT.
STUN_SERVERS = (
    ('stun.l.google.com', 19302),
    ('stun.cloudflare.com', 3478),
)

_MAGIC = 0x2112A442
_BINDING_REQUEST = 0x0001
_BINDING_RESPONSE = 0x0101
_XOR_MAPPED_ADDRESS = 0x0020

# Compressed version of the RFC 5389 retransmission schedule. The spec's full
# ladder runs to 39.5s, which is far longer than a pilot will wait for us.
_RETRANSMIT_DELAYS = (0.0, 0.25, 0.5, 1.0)


@dataclass
class NatDiscovery:
    """What STUN could work out about our position behind the NAT."""
    reflexive: Optional[tuple[str, int]] = None   # external ip:port, if consistent
    observed: list[tuple[str, int]] = field(default_factory=list)
    symmetric: bool = False

    @property
    def nat_type(self) -> str:
        if self.symmetric:
            return 'symmetric'
        if self.reflexive:
            return 'cone'
        return 'unknown'


# ── local candidates ─────────────────────────────────────────────────────────

def get_local_ips() -> list[str]:
    """
    Non-loopback local addresses, IPv4 and globally-routable IPv6.

    IPv6 is included deliberately: where both ends have it there is no NAT to
    traverse at all, which is the one path that reliably beats carrier-grade
    NAT on mobile links. Link-local (fe80::) and unique-local (fc00::/7) are
    filtered out — they are not reachable from a pilot elsewhere on the
    internet, and advertising them only wastes a connection attempt.
    """
    ips: list[str] = []
    try:
        import ifaddr
        for adapter in ifaddr.get_adapters():
            for ip in adapter.ips:
                if isinstance(ip.ip, str):
                    _append_if_routable(ips, ip.ip)          # IPv4
                elif isinstance(ip.ip, tuple) and ip.ip:
                    _append_if_routable(ips, ip.ip[0])       # IPv6 (addr, flowinfo, scope)
        if ips:
            return ips
    except ImportError:
        pass

    # Fallback: ask the kernel which source address it would use outbound.
    for family, probe in ((socket.AF_INET, '8.8.8.8'), (socket.AF_INET6, '2001:4860:4860::8888')):
        try:
            s = socket.socket(family, socket.SOCK_DGRAM)
            s.connect((probe, 80))
            _append_if_routable(ips, s.getsockname()[0])
            s.close()
        except OSError:
            pass
    return ips


def _append_if_routable(ips: list[str], raw: str):
    addr_str = raw.split('%', 1)[0]      # strip any zone id
    try:
        addr = ipaddress.ip_address(addr_str)
    except ValueError:
        return
    if addr.is_loopback or addr.is_link_local or addr.is_multicast:
        return
    # Private IPv4 is kept on purpose — that is the same-LAN candidate, and it
    # is the fastest path there is when the pilot happens to share the network.
    # IPv6 is only worth advertising when globally routable: ULA (fd00::/8) is
    # LAN-only like private IPv4 but virtually unused on consumer networks, so
    # it would just cost a wasted connection attempt.
    if isinstance(addr, ipaddress.IPv6Address) and not addr.is_global:
        return
    if str(addr) not in ips:
        ips.append(str(addr))


def is_ipv6(ip: str) -> bool:
    try:
        return isinstance(ipaddress.ip_address(ip), ipaddress.IPv6Address)
    except ValueError:
        return False


def format_host(ip: str) -> str:
    """Wrap IPv6 literals in brackets so they can go in a URL authority."""
    return f'[{ip}]' if is_ipv6(ip) else ip


# ── STUN over a socket we own ────────────────────────────────────────────────

async def _binding_transaction(sock: socket.socket, server: tuple[str, int],
                               timeout: float) -> Optional[tuple[str, int]]:
    """One Binding Request with retransmission; returns the reflexive address."""
    loop = asyncio.get_running_loop()

    try:
        info = await loop.getaddrinfo(server[0], server[1], type=socket.SOCK_DGRAM,
                                      family=sock.family)
        server_addr = info[0][4]
    except (OSError, socket.gaierror) as e:
        log.debug('STUN DNS lookup for %s failed: %s', server[0], e)
        return None

    transaction_id = secrets.token_bytes(12)
    request = struct.pack('!HHI', _BINDING_REQUEST, 0, _MAGIC) + transaction_id
    future: asyncio.Future = loop.create_future()

    def _recv():
        try:
            data, _ = sock.recvfrom(1024)
        except (BlockingIOError, OSError):
            return
        result = parse_binding_response(data, transaction_id)
        if result and not future.done():
            future.set_result(result)

    loop.add_reader(sock.fileno(), _recv)
    try:
        deadline = loop.time() + timeout
        for delay in _RETRANSMIT_DELAYS:
            if delay:
                try:
                    return await asyncio.wait_for(asyncio.shield(future), delay)
                except asyncio.TimeoutError:
                    pass
            if future.done():
                return future.result()
            if loop.time() >= deadline:
                break
            try:
                sock.sendto(request, server_addr)
            except OSError as e:
                log.debug('STUN send to %s failed: %s', server[0], e)
                return None
        remaining = max(0.0, deadline - loop.time())
        try:
            return await asyncio.wait_for(asyncio.shield(future), remaining)
        except asyncio.TimeoutError:
            log.debug('STUN timed out against %s', server[0])
            return None
    finally:
        loop.remove_reader(sock.fileno())


def parse_binding_response(data: bytes, transaction_id: bytes) -> Optional[tuple[str, int]]:
    """Extract XOR-MAPPED-ADDRESS from a Binding Success Response."""
    if len(data) < 20:
        return None
    msg_type, _msg_len, magic = struct.unpack_from('!HHI', data)
    if magic != _MAGIC or msg_type != _BINDING_RESPONSE or data[8:20] != transaction_id:
        return None

    offset = 20
    while offset + 4 <= len(data):
        attr_type, attr_len = struct.unpack_from('!HH', data, offset)
        offset += 4
        attr_data = data[offset:offset + attr_len]
        offset += (attr_len + 3) & ~3          # attributes are 4-byte aligned
        if attr_type != _XOR_MAPPED_ADDRESS or len(attr_data) < 8:
            continue

        family = attr_data[1]
        port = struct.unpack_from('!H', attr_data, 2)[0] ^ (_MAGIC >> 16)
        if family == 0x01:
            ip_int = struct.unpack_from('!I', attr_data, 4)[0] ^ _MAGIC
            return str(ipaddress.IPv4Address(ip_int)), port
        if family == 0x02 and len(attr_data) >= 20:
            # IPv6 is XORed with the magic cookie followed by the transaction id.
            mask = struct.pack('!I', _MAGIC) + transaction_id
            raw = bytes(a ^ b for a, b in zip(attr_data[4:20], mask))
            return str(ipaddress.IPv6Address(raw)), port
    return None


async def discover_nat(sock: socket.socket, timeout: float = 2.5) -> NatDiscovery:
    """
    Query every STUN server from `sock` and classify what we see.

    The socket must be bound and non-blocking, and must be the same one the
    QUIC server will go on to use — that is the whole point.
    """
    result = NatDiscovery()

    for server in STUN_SERVERS:
        addr = await _binding_transaction(sock, server, timeout)
        if addr:
            result.observed.append(addr)
            log.debug('STUN %s reports %s:%d', server[0], *addr)

    if not result.observed:
        log.warning('STUN failed against all %d servers — no reflexive candidate',
                    len(STUN_SERVERS))
        return result

    first = result.observed[0]
    if all(a == first for a in result.observed):
        result.reflexive = first
    elif len({a[0] for a in result.observed}) == 1:
        # Same external IP, different ports: textbook symmetric NAT. The port
        # is picked per destination, so no third party can be told where to
        # send. Only an explicit router mapping or a relay gets through.
        result.symmetric = True
        log.info('symmetric NAT detected (%s) — reflexive candidate is unusable',
                 ', '.join(f'{ip}:{p}' for ip, p in result.observed))
    else:
        # Different external IPs — multiple WAN links or a load-balanced CGNAT.
        # Treat the first as a best guess but flag it as unreliable.
        result.symmetric = True
        log.info('inconsistent external addresses from STUN (%s) — treating as symmetric',
                 ', '.join(f'{ip}:{p}' for ip, p in result.observed))

    return result
