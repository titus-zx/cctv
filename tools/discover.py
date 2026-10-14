#!/usr/bin/env python3
"""Read-only V380 discovery and bounded camera-port scan on explicit LAN subnets."""
import argparse, concurrent.futures, ipaddress, json, socket, threading, time

def discover(networks, timeout=0.8):
    hosts = [str(ip) for network in networks for ip in network.hosts()]
    devices, ports = {}, {}
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    udp.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    try: udp.bind(('', 10009))
    except OSError: udp.bind(('', 0))
    udp.settimeout(.2)
    stop = threading.Event()
    def receive():
        while not stop.is_set():
            try:
                data, sender = udp.recvfrom(4096)
                parts = data.decode('ascii', errors='replace').strip('\x00').split('^')
                if len(parts) >= 13 and parts[0] == 'NVDEVRESULT':
                    devices[sender[0]] = {'ip':sender[0], 'reportedIp':parts[3], 'deviceId':parts[12], 'mac':parts[2]}
            except socket.timeout: continue
            except OSError: break
    thread = threading.Thread(target=receive, daemon=True); thread.start()
    for target in ['255.255.255.255'] + [str(network.broadcast_address) for network in networks]:
        try: udp.sendto(b'NVDEVSEARCH^100', (target, 10008))
        except OSError: pass
    def check(ip):
        try: udp.sendto(b'NVDEVSEARCH^100', (ip, 10008))
        except OSError: pass
        found = []
        for port in [8800, 554]:
            try:
                with socket.create_connection((ip, port), timeout=timeout): found.append(port)
            except OSError: pass
        return ip, found
    with concurrent.futures.ThreadPoolExecutor(max_workers=32) as pool:
        for ip, found in pool.map(check, hosts):
            if found: ports[ip] = found
    # Repeat unicast to V380 endpoints in case broadcast was routed away.
    for ip, found in ports.items():
        if 8800 in found: udp.sendto(b'NVDEVSEARCH^100', (ip, 10008))
    time.sleep(1); stop.set(); thread.join(timeout=1); udp.close()
    return {'networks':[str(n) for n in networks], 'devices':sorted(devices.values(), key=lambda d:ipaddress.ip_address(d['ip'])),
        'openCameraPorts':[{'ip':ip,'ports':found} for ip, found in sorted(ports.items(),key=lambda x:ipaddress.ip_address(x[0]))]}

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('subnets',nargs='+');args = parser.parse_args()
    networks = [ipaddress.ip_network(n,strict=False) for n in args.subnets]
    if any(n.version != 4 or not n.is_private or n.num_addresses > 256 for n in networks):
        parser.error('Use private IPv4 LAN networks of /24 or smaller.')
    print(json.dumps(discover(networks),indent=2))
