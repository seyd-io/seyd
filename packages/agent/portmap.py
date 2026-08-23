"""
Router port mapping for darc-agent — PCP, NAT-PMP, and UPnP-IGD.

A robot behind a consumer router is unreachable by default: the pilot's browser
is a pure WebTransport *client*, so the agent has to be addressable as a server.
STUN plus hole punching only achieves that on full-cone and address-restricted
NATs. Asking the router for an explicit port mapping is strictly better — it
works on port-restricted and many symmetric NATs too, because the mapping is
installed rather than inferred, and it survives the mapping timeouts that make
hole punching flaky.

Three protocols are tried in order, newest first:

  PCP      (RFC 6887) — UDP to gateway:5351. The modern replacement for NAT-PMP.
  NAT-PMP  (RFC 6886) — UDP to gateway:5351. Apple's, widely deployed.
  UPnP-IGD (SSDP + SOAP) — the most common on consumer routers by far.

All three are implemented directly rather than via miniupnpc so the agent keeps
its dependency list to pure-Python wheels — this code has to cross-compile to
ARM embedded targets later.

Carrier-grade NAT (mobile networks) will refuse all three: there is no gateway
the subscriber controls. That refusal is fast and definitive, which is useful —
it tells the pilot to stop waiting for P2P.
"""

import asyncio
import ipaddress
import logging
import re
import secrets
import socket
import struct
import subprocess
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from typing import Optional

log = logging.getLogger(__name__)

_PCP_PORT = 5351
_SSDP_ADDR = ('239.255.255.250', 1900)


@dataclass
class PortMapping:
    """An external ip:port the router agreed to forward to us."""
    external_ip: str
    external_port: int
    protocol: str          # 'pcp' | 'nat-pmp' | 'upnp'
    lifetime: int          # seconds; 0 means the router calls it permanent

    @property
    def routable(self) -> bool:
        """
        Whether this address is actually reachable from the public internet.

        A router will happily install a mapping and report its own WAN address
        even when that address is itself behind another NAT — RFC 1918 on a
        double-NAT LAN, or 100.64.0.0/10 shared address space under carrier-grade
        NAT. The mapping is real and still useful (it removes the inner NAT from
        the path, which can make the STUN-reflexive candidate work), but the
        address must not be advertised to a pilot: nobody on the internet can
        reach it. `is_global` is the right predicate here and covers both
        families — note that `is_private` does *not* flag CGNAT space.
        """
        try:
            return ipaddress.ip_address(self.external_ip).is_global
        except ValueError:
            return False


# ── gateway discovery ────────────────────────────────────────────────────────

def default_gateway() -> Optional[str]:
    """Best-effort IPv4 default gateway, for the UDP-to-gateway protocols."""
    # Linux: parse the kernel routing table directly.
    try:
        with open('/proc/net/route') as f:
            next(f)
            for line in f:
                parts = line.split()
                if len(parts) > 2 and parts[1] == '00000000':
                    gw = struct.unpack('<L', bytes.fromhex(parts[2]))[0]
                    return str(ipaddress.IPv4Address(gw))
    except (OSError, StopIteration, ValueError):
        pass

    # macOS / BSD: `route -n get default` prints "gateway: 192.168.1.1".
    try:
        out = subprocess.run(['route', '-n', 'get', 'default'],
                             capture_output=True, text=True, timeout=3).stdout
        m = re.search(r'gateway:\s*([0-9.]+)', out)
        if m:
            return m.group(1)
    except (OSError, subprocess.SubprocessError):
        pass

    return None


def _local_ip_towards(host: str) -> Optional[str]:
    """Which of our addresses the kernel would use to reach `host`."""
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect((host, 9))
        ip = s.getsockname()[0]
        s.close()
        return ip
    except OSError:
        return None


async def _udp_request(gateway: str, payload: bytes, expect_len: int,
                       timeout: float = 1.0, attempts: int = 3) -> Optional[bytes]:
    """Send `payload` to gateway:5351 and await a reply, with retransmission."""
    loop = asyncio.get_running_loop()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setblocking(False)
    try:
        for _ in range(attempts):
            future: asyncio.Future = loop.create_future()

            def _recv():
                try:
                    data, _ = sock.recvfrom(1024)
                except BlockingIOError:
                    return
                if not future.done() and len(data) >= expect_len:
                    future.set_result(data)

            loop.add_reader(sock.fileno(), _recv)
            try:
                sock.sendto(payload, (gateway, _PCP_PORT))
                return await asyncio.wait_for(asyncio.shield(future), timeout)
            except asyncio.TimeoutError:
                continue
            except OSError:
                return None
            finally:
                loop.remove_reader(sock.fileno())
    finally:
        sock.close()
    return None


# ── PCP (RFC 6887) ───────────────────────────────────────────────────────────

async def _try_pcp(gateway: str, internal_port: int, lifetime: int) -> Optional[PortMapping]:
    client_ip = _local_ip_towards(gateway)
    if not client_ip:
        return None

    # PCP addresses are always 16 bytes; IPv4 goes in as ::ffff:a.b.c.d
    client_v6 = socket.inet_pton(socket.AF_INET6, '::ffff:' + client_ip)
    nonce = secrets.token_bytes(12)

    request = (
        struct.pack('!BBHI', 2, 1, 0, lifetime)   # version, MAP opcode, reserved, lifetime
        + client_v6
        + nonce
        + struct.pack('!B3xHH', 17, internal_port, internal_port)  # UDP, internal, suggested
        + b'\x00' * 16                            # suggested external address: any
    )

    reply = await _udp_request(gateway, request, expect_len=60)
    if not reply:
        return None

    version, opcode, _, result, _lifetime = struct.unpack_from('!BBBBI', reply, 0)
    if version != 2 or opcode != 0x81:
        return None
    if result != 0:
        log.debug('PCP refused: result=%d', result)
        return None

    granted_lifetime = _lifetime
    ext_port = struct.unpack_from('!H', reply, 42)[0]
    ext_raw = reply[44:60]
    ext_ip = _unpack_v6_or_mapped(ext_raw)
    if not ext_ip:
        return None
    return PortMapping(ext_ip, ext_port, 'pcp', granted_lifetime)


def _unpack_v6_or_mapped(raw: bytes) -> Optional[str]:
    try:
        addr = ipaddress.ip_address(raw)
    except ValueError:
        return None
    # PCP encodes IPv4 as an IPv4-mapped IPv6 address.
    if isinstance(addr, ipaddress.IPv6Address) and addr.ipv4_mapped:
        return str(addr.ipv4_mapped)
    return str(addr)


# ── NAT-PMP (RFC 6886) ───────────────────────────────────────────────────────

async def _try_natpmp(gateway: str, internal_port: int, lifetime: int) -> Optional[PortMapping]:
    # Opcode 1 = map UDP. Ask for the same external port we listen on.
    request = struct.pack('!BBHHHI', 0, 1, 0, internal_port, internal_port, lifetime)
    reply = await _udp_request(gateway, request, expect_len=16)
    if not reply:
        return None

    version, opcode, result, _epoch, _internal, ext_port, granted = \
        struct.unpack_from('!BBHIHHI', reply, 0)
    if version != 0 or opcode != 129 or result != 0:
        log.debug('NAT-PMP refused: result=%s', result)
        return None

    # External address needs a second request (opcode 0).
    addr_reply = await _udp_request(gateway, struct.pack('!BB', 0, 0), expect_len=12)
    if not addr_reply:
        return None
    a_version, a_opcode, a_result = struct.unpack_from('!BBH', addr_reply, 0)
    if a_version != 0 or a_opcode != 128 or a_result != 0:
        return None
    ext_ip = str(ipaddress.IPv4Address(addr_reply[8:12]))

    return PortMapping(ext_ip, ext_port, 'nat-pmp', granted)


# ── UPnP-IGD (SSDP discovery + SOAP control) ─────────────────────────────────

_SSDP_SEARCH = (
    'M-SEARCH * HTTP/1.1\r\n'
    'HOST: 239.255.255.250:1900\r\n'
    'MAN: "ssdp:discover"\r\n'
    'MX: 2\r\n'
    'ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n'
    '\r\n'
).encode()

_WAN_SERVICES = (
    'urn:schemas-upnp-org:service:WANIPConnection:1',
    'urn:schemas-upnp-org:service:WANPPPConnection:1',
)


async def _ssdp_discover(timeout: float = 3.0) -> Optional[str]:
    """Return the LOCATION URL of an InternetGatewayDevice, if one answers."""
    loop = asyncio.get_running_loop()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.settimeout(0)
    sock.setblocking(False)
    try:
        future: asyncio.Future = loop.create_future()

        def _recv():
            try:
                data, _ = sock.recvfrom(2048)
            except BlockingIOError:
                return
            m = re.search(rb'LOCATION:\s*(\S+)', data, re.IGNORECASE)
            if m and not future.done():
                future.set_result(m.group(1).decode())

        loop.add_reader(sock.fileno(), _recv)
        try:
            sock.sendto(_SSDP_SEARCH, _SSDP_ADDR)
            return await asyncio.wait_for(asyncio.shield(future), timeout)
        except (asyncio.TimeoutError, OSError):
            return None
        finally:
            loop.remove_reader(sock.fileno())
    finally:
        sock.close()


def _http(url: str, data: bytes = None, headers: dict = None, timeout: float = 5.0) -> Optional[bytes]:
    req = urllib.request.Request(url, data=data, headers=headers or {},
                                 method='POST' if data else 'GET')
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.read()
    except Exception as e:
        log.debug('UPnP HTTP %s failed: %s', url, e)
        return None


def _strip_ns(tag: str) -> str:
    return tag.split('}', 1)[-1]


def _find_control_url(desc_xml: bytes, base_url: str) -> Optional[tuple[str, str]]:
    """Locate a WAN connection service; return (controlURL, serviceType)."""
    try:
        root = ET.fromstring(desc_xml)
    except ET.ParseError:
        return None

    for service in root.iter():
        if _strip_ns(service.tag) != 'service':
            continue
        fields = {_strip_ns(c.tag): (c.text or '') for c in service}
        stype = fields.get('serviceType', '')
        if stype in _WAN_SERVICES and fields.get('controlURL'):
            return urllib.parse.urljoin(base_url, fields['controlURL']), stype
    return None


def _soap(control_url: str, service_type: str, action: str, body: str) -> Optional[bytes]:
    envelope = (
        '<?xml version="1.0"?>'
        '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" '
        's:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>'
        f'<u:{action} xmlns:u="{service_type}">{body}</u:{action}>'
        '</s:Body></s:Envelope>'
    ).encode()
    return _http(control_url, data=envelope, headers={
        'Content-Type': 'text/xml; charset="utf-8"',
        'SOAPAction': f'"{service_type}#{action}"',
    })


async def _try_upnp(internal_port: int, lifetime: int) -> Optional[PortMapping]:
    loop = asyncio.get_running_loop()

    location = await _ssdp_discover()
    if not location:
        return None

    desc = await loop.run_in_executor(None, _http, location)
    if not desc:
        return None

    found = _find_control_url(desc, location)
    if not found:
        log.debug('UPnP: no WAN connection service in device description')
        return None
    control_url, service_type = found

    internal_ip = _local_ip_towards(urllib.parse.urlparse(location).hostname or '192.168.0.1')
    if not internal_ip:
        return None

    def add(lease: int) -> Optional[bytes]:
        return _soap(control_url, service_type, 'AddPortMapping',
                     '<NewRemoteHost></NewRemoteHost>'
                     f'<NewExternalPort>{internal_port}</NewExternalPort>'
                     '<NewProtocol>UDP</NewProtocol>'
                     f'<NewInternalPort>{internal_port}</NewInternalPort>'
                     f'<NewInternalClient>{internal_ip}</NewInternalClient>'
                     '<NewEnabled>1</NewEnabled>'
                     '<NewPortMappingDescription>DARC agent</NewPortMappingDescription>'
                     f'<NewLeaseDuration>{lease}</NewLeaseDuration>')

    result = await loop.run_in_executor(None, add, lifetime)
    granted = lifetime
    if result is None:
        # Error 725 is OnlyPermanentLeasesSupported — plenty of routers only
        # accept a zero lease. Retry permanently rather than giving up.
        result = await loop.run_in_executor(None, add, 0)
        granted = 0
    if result is None:
        return None

    ext = await loop.run_in_executor(
        None, _soap, control_url, service_type, 'GetExternalIPAddress', '')
    if not ext:
        return None
    m = re.search(rb'<NewExternalIPAddress>([^<]+)</NewExternalIPAddress>', ext)
    if not m:
        return None

    return PortMapping(m.group(1).decode().strip(), internal_port, 'upnp', granted)


# ── public entry point ───────────────────────────────────────────────────────

async def map_port(internal_port: int, lifetime: int = 3600) -> Optional[PortMapping]:
    """
    Ask the router to forward `internal_port/UDP` to us.

    Returns the resulting external address, or None when no protocol succeeds
    (no gateway, CGNAT, UPnP disabled, or double NAT). Never raises.
    """
    gateway = default_gateway()

    def report(mapping: PortMapping) -> PortMapping:
        if mapping.routable:
            log.info('%s mapped %s:%d → us (lifetime %ds)', mapping.protocol,
                     mapping.external_ip, mapping.external_port, mapping.lifetime)
        else:
            log.info('%s mapped port %d, but the router\'s external address (%s) is '
                     'not globally reachable — double NAT or CGNAT above it. The '
                     'mapping still removes the local NAT from the path.',
                     mapping.protocol, mapping.external_port, mapping.external_ip)
        return mapping

    attempts = []
    if gateway:
        attempts += [('PCP', lambda: _try_pcp(gateway, internal_port, lifetime)),
                     ('NAT-PMP', lambda: _try_natpmp(gateway, internal_port, lifetime))]
    else:
        log.debug('no default gateway found — skipping PCP/NAT-PMP')
    attempts.append(('UPnP', lambda: _try_upnp(internal_port, lifetime)))

    for name, attempt in attempts:
        try:
            mapping = await attempt()
        except Exception as e:
            log.debug('%s failed: %s', name, e)
            continue
        if mapping:
            return report(mapping)

    log.info('no router port mapping available (PCP, NAT-PMP and UPnP all declined)')
    return None
