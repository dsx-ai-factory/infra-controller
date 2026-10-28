#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""SSH to one native lab guest through its target pod; keep the key on the client."""

import argparse
import ipaddress
import json
import os
from pathlib import Path
import shlex
import subprocess
import stat

# kubectl's stdin/stdout carry SSH bytes. A write-side EOF must still allow the
# guest to send its final response; no SSH client or private key enters the pod.
RELAY = '''import os,select,socket,sys
with socket.create_connection((sys.argv[1],int(sys.argv[2])),timeout=10) as connection:
 connection.settimeout(None)
 inputs=[connection,sys.stdin.buffer]
 while True:
  readable,_,_=select.select(inputs,[],[])
  for source in readable:
   data=connection.recv(65536) if source is connection else os.read(source.fileno(),65536)
   if not data:
    if source is connection: sys.exit(0)
    inputs.remove(source); connection.shutdown(socket.SHUT_WR)
   elif source is connection:
    sys.stdout.buffer.write(data); sys.stdout.buffer.flush()
   else: connection.sendall(data)
'''


def literal_ssh_path(path):
    value = str(path.resolve())
    if any(character.isspace() or character in '\"\'%$\\' for character in value):
        raise ValueError('SSH file paths must not contain whitespace, quotes, %, $ or backslashes')
    return value


def ssh_command(args, kube, address):
    proxy = kube + ['exec', '-i', 'deployment/' + args.release, '--', 'python3', '-c', RELAY, str(address), '22']
    return ['ssh', '-F', '/dev/null', '-i', literal_ssh_path(args.identity), '-o', 'IdentitiesOnly=yes',
            '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=accept-new',
            '-o', 'UserKnownHostsFile=' + literal_ssh_path(args.known_hosts),
            '-o', 'GlobalKnownHostsFile=/dev/null', '-o', 'ConnectTimeout=15', '-o', 'ProxyCommand=' + shlex.join(proxy),
            '--', args.user + '@' + str(address)] + args.command


def prepare_known_hosts(path):
    # Open without truncation to prove that pinning can persist. OpenSSH only
    # warns on a failed host-key write and can otherwise keep accepting new keys.
    descriptor = os.open(literal_ssh_path(path), os.O_RDWR | os.O_CREAT, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise ValueError('--known-hosts must be a regular writable file')
    finally:
        os.close(descriptor)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--context', required=True)
    parser.add_argument('--namespace', default='bmc-lab')
    parser.add_argument('--release', default='target-one')
    parser.add_argument('--address', required=True, type=ipaddress.ip_address)
    parser.add_argument('--identity', required=True, type=Path)
    parser.add_argument('--known-hosts', required=True, type=Path)
    parser.add_argument('--user', default='lab')
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not args.context.startswith('kind-'):
        parser.error('select an explicit kind context')
    try:
        literal_ssh_path(args.identity)
        literal_ssh_path(args.known_hosts)
    except ValueError as error:
        parser.error(str(error))
    if not args.identity.is_file():
        parser.error('--identity must be an existing private key file')
    kube = ['kubectl', '--context', args.context, '-n', args.namespace]
    deployment = json.loads(subprocess.check_output(kube + ['get', 'deployment', args.release, '-o', 'json']))
    if deployment['metadata']['labels'].get('app.kubernetes.io/name') != 'nico-bmc-target':
        parser.error('selected deployment is not a native BMC test target')
    inventory = json.loads(subprocess.check_output(kube + ['exec', 'deployment/' + args.release,
                                                          '--', 'cat', '/data/inventory.json']))
    bridge_address = inventory.get('bridgeAddress' if args.address.version == 4 else 'bridgeAddressV6')
    if not bridge_address:
        parser.error('selected target has no bridge subnet for this address family')
    bridge = ipaddress.ip_interface(bridge_address)
    reserved = {bridge.ip, bridge.network.network_address}
    if bridge.version == 4:
        reserved.add(bridge.network.broadcast_address)
    if args.address not in bridge.network or args.address in reserved or getattr(args.address, 'scope_id', None):
        parser.error('--address must be a guest address in the selected target bridge subnet')
    try:
        prepare_known_hosts(args.known_hosts)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    if args.command[:1] == ['--']:
        args.command = args.command[1:]
    raise SystemExit(subprocess.call(ssh_command(args, kube, args.address)))


if __name__ == '__main__':
    main()
