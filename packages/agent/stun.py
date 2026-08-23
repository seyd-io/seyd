"""
Async STUN client (RFC 5389) + local candidate discovery.

STUN uses a temporary socket bound to the same local port as the WebTransport
server. The NAT mapping created is for that specific port, and most residential
NATs preserve the mapping when a new socket quickly rebinds the same local port
(port-preservation behaviour). Local LAN IP candidates are always collected
first so same-network connections bypass NAT entirely.
"""

import asyncio
import ipaddress
import logging
import secrets
import socket
import struct

log = logging.getLogger(__name__)

_STUN_HOST = 'stun.l.google.com'
_STUN_PORT = 19302
_MAGIC     = 0x2112A442


def get_local_ips() -> list[str]:
    """Return all non-loopback IPv4 addresses on this host."""
    ips = []
    try:
        import ifaddr
        for adapter in ifaddr.get_adapters():
            for ip in adapter.ips:
                if isinstance(ip.ip, str):
                    try:
                        addr = ipaddress.IPv4Address(ip.ip)
                        if not addr.is_loopback and not addr.is_link_local:
                            ips.append(str(addr))
                    except ValueError:
                        pass
        if ips:
            return ips
    except ImportError:
        pass

    # Fallback: connect a dummy socket to determine the outbound interface IP.
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(('8.8.8.8', 80))
        ips.append(s.getsockname()[0])
        s.close()
    except Exception:
        pass
    return ips


async def get_stun_address(local_port: int, timeout: float = 5.0) -> tuple[str, int] | None:
    """
    Discover the public IP:port for local UDP port `local_port`.

    Creates a temporary UDP socket, binds it to the same port as the
    WebTransport server (using SO_REUSEPORT / SO_REUSEADDR), sends a STUN
    Binding Request, waits for the response, and immediately closes the socket.

    The NAT mapping created by this socket is for local_port. Many NATs apply
    port-preservation: when aioquic rebinds the same local_port immediately
    after, the NAT reuses the same external port entry.
    """
    loop = asyncio.get_running_loop()

    try:
        stun_info = await loop.getaddrinfo(_STUN_HOST, _STUN_PORT, type=socket.SOCK_DGRAM)
        stun_addr = stun_info[0][4]
    except Exception as e:
        log.warning('STUN DNS lookup failed: %s', e)
        return None

    transaction_id = secrets.token_bytes(12)
    request = struct.pack('!HHI', 0x0001, 0, _MAGIC) + transaction_id

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        # SO_REUSEADDR lets us bind the same port the WebTransport server will use
        # (or may already be using if called post-start — but we call pre-start).
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        if hasattr(socket, 'SO_REUSEPORT'):
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
        sock.bind(('0.0.0.0', local_port))
        sock.setblocking(False)

        future: asyncio.Future = loop.create_future()

        def _recv():
            try:
                data, _ = sock.recvfrom(512)
            except BlockingIOError:
                return
            _parse(data, transaction_id, future)

        loop.add_reader(sock.fileno(), _recv)
        try:
            sock.sendto(request, stun_addr)
            return await asyncio.wait_for(asyncio.shield(future), timeout)
        except (asyncio.TimeoutError, Exception) as e:
            log.warning('STUN failed: %s', e)
            return None
        finally:
            loop.remove_reader(sock.fileno())
    finally:
        sock.close()


def _parse(data: bytes, transaction_id: bytes, future: asyncio.Future):
    if len(data) < 20 or future.done():
        return
    msg_type, msg_len, magic = struct.unpack_from('!HHI', data)
    if magic != _MAGIC or msg_type != 0x0101 or data[8:20] != transaction_id:
        return
    offset = 20
    while offset + 4 <= len(data):
        attr_type, attr_len = struct.unpack_from('!HH', data, offset)
        offset += 4
        attr_data = data[offset:offset + attr_len]
        offset += (attr_len + 3) & ~3
        if attr_type == 0x0020 and len(attr_data) >= 8 and attr_data[1] == 0x01:
            port   = struct.unpack_from('!H', attr_data, 2)[0] ^ (_MAGIC >> 16)
            ip_int = struct.unpack_from('!I', attr_data, 4)[0] ^ _MAGIC
            ip     = '.'.join(str((ip_int >> (24 - 8 * i)) & 0xFF) for i in range(4))
            future.set_result((ip, port))
            return
    if not future.done():
        future.set_exception(RuntimeError('XOR-MAPPED-ADDRESS not found'))
