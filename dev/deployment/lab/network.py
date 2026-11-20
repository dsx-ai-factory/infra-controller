# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Owned guest forwarding, DHCP relays and managed IPv6 router advertisements."""

import ipaddress
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys


class Network:
    def __init__(self, config, run, server='', pod_cidr='', uplink='eth0', server_v6='', default_route_v6=False, pod_cidr_v6=''):
        self.config = config
        self.run = run
        self.owner = 'nico-target:' + config['uuid']
        self.subnet = ipaddress.IPv4Interface(config['bridgeAddress']).network
        self.subnet_v6 = (ipaddress.IPv6Interface(config['bridgeAddressV6']).network
                          if config.get('bridgeAddressV6') else None)
        self.server_v6 = ipaddress.IPv6Address(server_v6) if server_v6 else None
        self.pod_cidr_v6 = ipaddress.IPv6Network(pod_cidr_v6) if pod_cidr_v6 else None
        if (self.server_v6 is None) != (self.pod_cidr_v6 is None):
            raise ValueError('DHCPv6 server and IPv6 pod CIDR must be supplied together')
        if self.pod_cidr_v6 is not None and (self.pod_cidr_v6.prefixlen == 0
                or self.subnet_v6 is not None and self.pod_cidr_v6.overlaps(self.subnet_v6)):
            raise ValueError('IPv6 pod CIDR must be non-default and separate from the guest subnet')
        self.default_route_v6 = default_route_v6
        if default_route_v6 and self.server_v6 is None:
            raise ValueError('IPv6 default-route advertisements require DHCPv6')
        if self.server_v6 is not None:
            if (self.subnet_v6 is None or self.server_v6.scope_id is not None
                    or self.server_v6 in self.subnet_v6
                    or not (self.server_v6 in ipaddress.IPv6Network('fc00::/7')
                            or self.server_v6 in ipaddress.IPv6Network('2000::/3'))):
                raise ValueError('DHCPv6 requires an IPv6 bridge and an unscoped ULA/global server outside its subnet')
        self.enabled = bool(server or pod_cidr)
        self.uplink = uplink
        self.relay = None
        if not self.enabled:
            return
        self.server = ipaddress.IPv4Address(server)
        self.pod_cidr = ipaddress.IPv4Network(pod_cidr)
        if (self.server.is_unspecified or self.server.is_multicast or self.server.is_loopback
                or self.server.is_link_local or self.server == ipaddress.IPv4Address('255.255.255.255')
                or self.server in self.subnet):
            raise ValueError('DHCP server must be a unicast IPv4 address outside the guest subnet')
        if self.pod_cidr.prefixlen == 0 or self.pod_cidr.overlaps(self.subnet):
            raise ValueError('pod CIDR must be non-default and separate from the guest subnet')
        if not re.fullmatch(r'[a-zA-Z0-9_.:-]{1,15}', uplink) or uplink == config['bridge']:
            raise ValueError('uplink must name a separate Linux interface')

    def owned_rules(self, tool='iptables', subnet=None):
        subnet = self.subnet if subnet is None else subnet
        result = []
        for table in ('filter', 'nat'):
            for line in self.run(tool + '-save', '-t', table).splitlines():
                fields = shlex.split(line)
                if '--comment' not in fields or fields[fields.index('--comment') + 1] != self.owner:
                    continue
                chain = 'FORWARD' if table == 'filter' else 'POSTROUTING'
                if fields[:2] != ['-A', chain] or '-s' not in fields and '-d' not in fields:
                    raise ValueError('unrecognized rule uses this target ownership tag')
                addresses = [fields[i + 1] for i, field in enumerate(fields) if field in ('-s', '-d')]
                if str(subnet) not in addresses:
                    raise ValueError('owned rule does not match the retained guest subnet')
                result.append((table, fields[1], fields[2:]))
        return result

    def preflight(self):
        if self.subnet_v6 is not None:
            if Path('/proc/sys/net/ipv6/conf/all/forwarding').read_text().strip() != '1':
                raise ValueError('the node must already enable IPv6 forwarding')
            for interface in ('all', 'default', self.config['bridge']):
                path = Path('/proc/sys/net/ipv6/conf') / interface / 'disable_ipv6'
                if path.exists() and path.read_text().strip() != '0':
                    raise ValueError('IPv6 must be enabled on the node and guest bridge')
            self.owned_rules('ip6tables', self.subnet_v6)
        if self.server_v6 is not None and ':KUBE-POSTROUTING ' not in self.run('ip6tables-save', '-t', 'nat'):
            raise ValueError("DHCPv6 requires kube-proxy in iptables mode on the image's nf_tables backend")
        if not self.enabled:
            return
        if Path('/proc/sys/net/ipv4/ip_forward').read_text().strip() != '1':
            raise ValueError('the node must already enable IPv4 forwarding')
        links = json.loads(self.run('ip', '-json', 'link', 'show', 'dev', self.uplink))
        if not links or 'UP' not in links[0].get('flags', []):
            raise ValueError('the provisioning uplink must exist and be up')
        if ':KUBE-POSTROUTING ' not in self.run('iptables-save', '-t', 'nat'):
            raise ValueError("provisioning requires kube-proxy in iptables mode on the image's nf_tables backend")
        self.owned_rules()

    def clear(self):
        # Read and validate the complete owned set before deleting any rule.
        rules = [('iptables', *rule) for rule in self.owned_rules()]
        if self.subnet_v6 is not None:
            rules.extend(('ip6tables', *rule) for rule in self.owned_rules('ip6tables', self.subnet_v6))
        for tool, table, chain, rule in rules:
            self.run(tool, '-w', '5', '-t', table, '-D', chain, *rule)

    def rules(self):
        tag = ['-m', 'comment', '--comment', self.owner]
        bridge = self.config['bridge']
        subnet = str(self.subnet)
        yield 'filter', 'FORWARD', ['-i', bridge, '-s', subnet, *tag, '-j', 'ACCEPT']
        yield 'filter', 'FORWARD', ['-o', bridge, '-d', subnet, '-m', 'conntrack',
                                    '--ctstate', 'RELATED,ESTABLISHED', *tag, '-j', 'ACCEPT']
        yield 'nat', 'POSTROUTING', ['-s', subnet, '-o', self.uplink, *tag, '-j', 'MASQUERADE']
        # Insert exemptions after masquerade so they precede it in POSTROUTING.
        # DNAT has already selected the pod; Scout must retain the guest source.
        yield 'nat', 'POSTROUTING', ['-s', subnet, '-d', str(self.pod_cidr), *tag, '-j', 'ACCEPT']
        # kindnet also masquerades non-pod destinations. Preserve peer sources
        # only when the node delivers traffic into this target's own bridge.
        yield 'nat', 'POSTROUTING', ['-d', subnet, '-o', bridge, *tag, '-j', 'ACCEPT']

    def ipv6_rules(self):
        tag = ['-m', 'comment', '--comment', self.owner]
        bridge, subnet = self.config['bridge'], str(self.subnet_v6)
        yield 'filter', 'FORWARD', ['-i', bridge, '-s', subnet, *tag, '-j', 'ACCEPT']
        yield 'filter', 'FORWARD', ['-o', bridge, '-d', subnet, '-m', 'conntrack',
                                    '--ctstate', 'RELATED,ESTABLISHED', *tag, '-j', 'ACCEPT']
        if self.pod_cidr_v6 is not None:
            # Kea sends relay replies to UDP 547; SNAT port remapping loses them.
            yield 'nat', 'POSTROUTING', ['-s', subnet, '-d', str(self.pod_cidr_v6), *tag, '-j', 'ACCEPT']
        # Preserve peer sources even when the cluster has an IPv6 masquerade rule.
        yield 'nat', 'POSTROUTING', ['-d', subnet, '-o', bridge, *tag, '-j', 'ACCEPT']

    def upstreams(self):
        # kindnet gives each pod-facing veth an address from the node pod CIDR.
        # These links change when pods are replaced; eth0 alone misses replies.
        links = json.loads(self.run('ip', '-json', 'address', 'show'))
        return sorted({self.uplink} | {
            link['ifname'] for link in links if 'UP' in link.get('flags', [])
            and any(address['family'] == 'inet'
                    and ipaddress.IPv4Address(address['local']) in self.pod_cidr
                    for address in link.get('addr_info', []))})

    def refresh(self):
        if self.relay is not None:
            self.relay.refresh(self.upstreams())

    def router_advertisement(self):
        return f"""interface {self.config['bridge']} {{
    AdvSendAdvert on;
    MinRtrAdvInterval 3;
    MaxRtrAdvInterval 10;
    AdvManagedFlag on;
    AdvOtherConfigFlag on;
    AdvDefaultLifetime {30 if self.default_route_v6 else 0};
    prefix {self.subnet_v6} {{
        AdvOnLink on;
        AdvAutonomous off;
    }};
}};
"""

    def start(self):
        # Also removes stale owned rules when provisioning is disabled after a crash.
        self.clear()
        children = []
        try:
            if self.subnet_v6 is not None:
                for table, chain, rule in self.ipv6_rules():
                    self.run('ip6tables', '-w', '5', '-t', table, '-I', chain, '1', *rule)
            if self.enabled:
                for table, chain, rule in self.rules():
                    self.run('iptables', '-w', '5', '-t', table, '-I', chain, '1', *rule)
                self.relay = Relay(self.config['bridge'], str(self.server), self.upstreams())
                children.append(self.relay)
            if self.server_v6 is not None:
                # Each target has its own mount namespace and retained bridge identity.
                path = Path('/run/nico-radvd.conf')
                path.write_text(self.router_advertisement())
                children.append(subprocess.Popen([
                    sys.executable, '-u', str(Path(__file__).with_name('dhcp6_relay.py')),
                    '--interface', self.config['bridge'],
                    '--link-address', str(ipaddress.IPv6Interface(self.config['bridgeAddressV6']).ip),
                    '--server', str(self.server_v6), '--mac', self.config['mac']]))
                children.append(subprocess.Popen(['radvd', '-n', '-m', 'stderr', '-C', str(path),
                                                 '-p', '/run/nico-radvd.pid']))
            return children
        except (OSError, subprocess.SubprocessError):
            for child in reversed(children):
                stop_child(child)
            self.clear()
            raise


def stop_child(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=20)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)


class Relay:
    """Supervised ISC relay that follows kindnet pod-interface replacement."""

    def __init__(self, bridge, server, upstreams):
        self.bridge = bridge
        self.server = server
        self.upstreams = upstreams
        self.process = self.start()

    def start(self):
        arguments = ['dhcrelay', '-4', '-d', '-pf', '/run/nico-dhcrelay.pid', '-id', self.bridge]
        for interface in self.upstreams:
            arguments.extend(['-iu', interface])
        return subprocess.Popen([*arguments, self.server])

    def refresh(self, upstreams):
        # A real process failure is handled by the target supervisor, not retried.
        if self.process.poll() is not None or upstreams == self.upstreams:
            return
        self.terminate()
        try:
            self.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.kill()
            self.wait(timeout=5)
        self.upstreams = upstreams
        self.process = self.start()

    def poll(self):
        return self.process.poll()

    def terminate(self):
        self.process.terminate()

    def wait(self, timeout):
        return self.process.wait(timeout=timeout)

    def kill(self):
        self.process.kill()
