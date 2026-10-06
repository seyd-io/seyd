#!/usr/bin/env python3
# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""Find an IP camera on the local network.

Three independent methods, because each fails differently:

  SADP   Hikvision's own discovery protocol (UDP multicast 239.255.255.250:37020).
         Finds a camera even when its IP is on the wrong subnet, and even when it
         is still un-activated -- but only across a single layer-2 segment, and
         some routers/APs drop the multicast.
  ONVIF  WS-Discovery (UDP multicast 239.255.255.250:3702). Vendor-neutral. Same
         layer-2 limitation, and ONVIF is off by default on some firmware.
  SCAN   Direct TCP connect to the camera service ports across a subnet. Works
         through routers, so it is the only method that crosses a NAT boundary --
         but it needs the right subnet and it cannot see an off-subnet static IP.

Nothing here is DARC. This is a setup aid for pointing the agent at a camera;
it is never imported by packages/.

Usage:
    tools/find-camera.py                  # discovery on every local interface
    tools/find-camera.py --scan           # also port-scan each interface's /24
    tools/find-camera.py --scan 192.168.1.0/24   # port-scan a specific subnet
    tools/find-camera.py --timeout 20     # listen longer for slow devices
"""

import argparse
import concurrent.futures
import ipaddress
import socket
import struct
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

SADP_GROUP = "239.255.255.250"
SADP_PORT = 37020
WSD_PORT = 3702

# Ports that identify a camera rather than a general-purpose host. 554 (RTSP) is
# the one that matters for DARC; the rest disambiguate vendor and confirm a hit.
CAMERA_PORTS = {
    554: "RTSP",
    8000: "Hikvision SDK",
    80: "HTTP",
    443: "HTTPS",
    37777: "Dahua SDK",
    2020: "ONVIF (alt)",
    8899: "ONVIF (alt)",
}

# Presence of a camera service port, not just a web server, is what counts as a
# hit -- otherwise every router and printer on the LAN reports as a camera.
STRONG_PORTS = {554, 8000, 37777, 2020, 8899}


def local_ipv4_interfaces():
    """Return [(ifname, ip, prefixlen)] for every up IPv4 interface."""
    out = []
    try:
        names = subprocess.run(
            ["ifconfig", "-l"], capture_output=True, text=True, timeout=5
        ).stdout.split()
    except Exception:
        return out

    for name in names:
        if name.startswith(("lo", "utun", "gif", "stf", "awdl", "llw", "ipsec")):
            continue
        try:
            detail = subprocess.run(
                ["ifconfig", name], capture_output=True, text=True, timeout=5
            ).stdout
        except Exception:
            continue
        for line in detail.splitlines():
            line = line.strip()
            if not line.startswith("inet ") or "127.0.0.1" in line:
                continue
            parts = line.split()
            ip = parts[1]
            prefix = 24
            if "netmask" in parts:
                mask_hex = parts[parts.index("netmask") + 1]
                try:
                    mask = int(mask_hex, 16)
                    prefix = bin(mask).count("1")
                except ValueError:
                    pass
            out.append((name, ip, prefix))
            break
    return out


def sadp_probe(local_ip, timeout):
    """Send SADP inquiries and collect replies. Returns {ip: {field: value}}."""
    results = {}
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock.bind(("", SADP_PORT))
    except OSError as exc:
        print(f"  SADP: cannot bind :{SADP_PORT} ({exc}) -- another tool running?")
        return results

    try:
        mreq = struct.pack(
            "4s4s", socket.inet_aton(SADP_GROUP), socket.inet_aton(local_ip)
        )
        sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, mreq)
        sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton(local_ip))
        sock.settimeout(0.5)

        probe = (
            '<?xml version="1.0" encoding="utf-8"?>'
            "<Probe><Uuid>DARC0000-0000-4000-8000-000000000001</Uuid>"
            "<Types>inquiry</Types></Probe>"
        ).encode()

        deadline = time.time() + timeout
        last_send = 0.0
        while time.time() < deadline:
            if time.time() - last_send > 2.0:
                try:
                    sock.sendto(probe, (SADP_GROUP, SADP_PORT))
                except OSError:
                    pass
                last_send = time.time()
            try:
                data, addr = sock.recvfrom(8192)
            except socket.timeout:
                continue
            # We receive our own multicast back; a Probe is not a device.
            text = data.decode("utf-8", "replace")
            if "<Probe>" in text or addr[0] == local_ip:
                continue
            info = _parse_sadp(text)
            if info:
                results.setdefault(info.get("ip", addr[0]), {}).update(info)
    finally:
        sock.close()
    return results


def _parse_sadp(text):
    """Pull the useful fields out of a SADP ProbeMatch."""
    field_map = {
        "IPv4Address": "ip",
        "DeviceDescription": "model",
        "DeviceSN": "serial",
        "MAC": "mac",
        "SoftwareVersion": "firmware",
        "Activated": "activated",
        "HttpPort": "http_port",
        "CommandPort": "sdk_port",
        "IPv4SubnetMask": "netmask",
        "IPv4Gateway": "gateway",
        "DHCP": "dhcp",
    }
    try:
        root = ET.fromstring(text)
    except ET.ParseError:
        return {}
    info = {}
    for elem in root.iter():
        key = field_map.get(elem.tag)
        if key and elem.text:
            info[key] = elem.text.strip()
    return info


def onvif_probe(local_ip, timeout):
    """WS-Discovery Probe for NetworkVideoTransmitter. Returns {ip: {...}}."""
    results = {}
    msg = (
        '<?xml version="1.0" encoding="UTF-8"?>'
        '<e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope" '
        'xmlns:w="http://schemas.xmlsoap.org/ws/2004/08/addressing" '
        'xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery" '
        'xmlns:dn="http://www.onvif.org/ver10/network/wsdl">'
        "<e:Header>"
        "<w:MessageID>uuid:daac0000-0000-4000-8000-000000000002</w:MessageID>"
        '<w:To e:mustUnderstand="true">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To>'
        '<w:Action e:mustUnderstand="true">'
        "http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action>"
        "</e:Header>"
        "<e:Body><d:Probe><d:Types>dn:NetworkVideoTransmitter</d:Types></d:Probe></e:Body>"
        "</e:Envelope>"
    ).encode()

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock.bind((local_ip, 0))
        sock.settimeout(0.5)
        deadline = time.time() + timeout
        last_send = 0.0
        while time.time() < deadline:
            if time.time() - last_send > 2.0:
                try:
                    sock.sendto(msg, (SADP_GROUP, WSD_PORT))
                except OSError:
                    pass
                last_send = time.time()
            try:
                data, addr = sock.recvfrom(16384)
            except socket.timeout:
                continue
            text = data.decode("utf-8", "replace")
            if "Probe>" in text and "ProbeMatch" not in text:
                continue
            entry = results.setdefault(addr[0], {"ip": addr[0]})
            for line in text.replace("><", ">\n<").splitlines():
                if "http://" in line and "XAddrs" in line:
                    entry["onvif_xaddr"] = line.split(">", 1)[-1].split("<")[0].strip()
            entry["onvif"] = "yes"
    finally:
        sock.close()
    return results


def port_scan(network, timeout=1.0, workers=128):
    """TCP-connect scan a subnet for camera ports. Returns {ip: [(port, name)]}."""
    net = ipaddress.ip_network(network, strict=False)
    if net.num_addresses > 1024:
        print(
            f"  refusing to scan {net} -- {net.num_addresses} addresses is too "
            "broad. Narrow it to a /24 or smaller."
        )
        return {}

    hosts = [str(h) for h in net.hosts()]
    tasks = [(h, p) for h in hosts for p in CAMERA_PORTS]

    def probe(task):
        host, port = task
        sock = socket.socket()
        sock.settimeout(timeout)
        try:
            sock.connect((host, port))
            return host, port
        except Exception:
            return None
        finally:
            sock.close()

    hits = {}
    with concurrent.futures.ThreadPoolExecutor(workers) as pool:
        for result in pool.map(probe, tasks):
            if result:
                host, port = result
                hits.setdefault(host, []).append((port, CAMERA_PORTS[port]))

    # Drop hosts that only answered on 80/443 -- routers, printers, NAS boxes.
    return {
        host: sorted(ports)
        for host, ports in hits.items()
        if any(p in STRONG_PORTS for p, _ in ports)
    }


def rtsp_url_hint(ip, port=554):
    return (
        f"rtsp://<user>:<pass>@{ip}:{port}/Streaming/Channels/101   (Hikvision main)\n"
        f"    rtsp://<user>:<pass>@{ip}:{port}/Streaming/Channels/102   (Hikvision sub)"
    )


def main():
    ap = argparse.ArgumentParser(description="Find an IP camera on the local network.")
    ap.add_argument(
        "--scan",
        nargs="?",
        const="auto",
        metavar="CIDR",
        help="also TCP-scan for camera ports; CIDR, or omit to use each interface's /24",
    )
    ap.add_argument("--timeout", type=float, default=10.0, help="discovery listen seconds")
    args = ap.parse_args()

    interfaces = local_ipv4_interfaces()
    if not interfaces:
        print("No usable IPv4 interface found.")
        return 1

    print("Local interfaces:")
    for name, ip, prefix in interfaces:
        print(f"  {name}  {ip}/{prefix}")
    print()

    found = {}

    for name, ip, prefix in interfaces:
        print(f"[{name} {ip}] SADP + ONVIF discovery ({args.timeout:.0f}s)...")
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            sadp_future = pool.submit(sadp_probe, ip, args.timeout)
            onvif_future = pool.submit(onvif_probe, ip, args.timeout)
            for source, results in (
                ("SADP", sadp_future.result()),
                ("ONVIF", onvif_future.result()),
            ):
                for host, info in results.items():
                    entry = found.setdefault(host, {"ip": host, "via": set()})
                    entry["via"].add(source)
                    entry.update({k: v for k, v in info.items() if k != "ip"})
                if results:
                    print(f"  {source}: {len(results)} device(s)")

        if args.scan:
            target = f"{ip}/{prefix}" if args.scan == "auto" else args.scan
            print(f"  scanning {target} for camera ports...")
            for host, ports in port_scan(target).items():
                entry = found.setdefault(host, {"ip": host, "via": set()})
                entry["via"].add("SCAN")
                entry["ports"] = ", ".join(f"{p} {n}" for p, n in ports)
            if args.scan != "auto":
                break  # an explicit CIDR is scanned once, not per interface
        print()

    print("=" * 68)
    if not found:
        print("No cameras found.\n")
        print("SADP and ONVIF only cross a single layer-2 segment, so the most")
        print("common cause is the camera being on a different subnet than this")
        print("machine -- a second router, or a switch behind another NAT. Check:")
        print("  * camera and this machine on the SAME router/switch, not across two")
        print("  * upstream router's DHCP client list for the camera's lease")
        print("  * a factory-static camera: tools/find-camera.py --scan 192.168.1.0/24")
        print("  * PoE actually delivering power (802.3af) -- link LED and PTZ sweep")
        return 1

    print(f"Found {len(found)} camera candidate(s):\n")
    for host, info in sorted(found.items(), key=lambda kv: ipaddress.ip_address(kv[0])):
        print(f"  {host}   via {'+'.join(sorted(info['via']))}")
        for key in (
            "model",
            "firmware",
            "serial",
            "mac",
            "activated",
            "dhcp",
            "netmask",
            "gateway",
            "http_port",
            "sdk_port",
            "ports",
            "onvif_xaddr",
        ):
            if info.get(key):
                print(f"      {key:<12} {info[key]}")
        if info.get("activated", "true").lower() in ("false", "no", "0"):
            print("      NOTE         device is NOT activated -- set a password first")
        print(f"      rtsp         {rtsp_url_hint(host)}")
        print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
