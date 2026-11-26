#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Verify direct IPv4 or IPv6 TCP connectivity and unmodified source addresses between two lab guests."""

import argparse
from contextlib import contextmanager
import ipaddress
import json
import os
from pathlib import Path
import secrets
import select
import shlex
import signal
import subprocess
import sys
import time

# These programs run in the installed guests, not in the relay pods. Each
# listener accepts one connection and expires even if the controlling SSH dies.
SERVER = '''import json,socket,sys
address,expected_source,token=sys.argv[1:]
with socket.socket(socket.AF_INET6 if ':' in address else socket.AF_INET) as listener:
 listener.settimeout(30)
 listener.bind((address,0))
 listener.listen(1)
 print(json.dumps({'port':listener.getsockname()[1]}),flush=True)
 with listener.accept()[0] as connection:
  connection.settimeout(10)
  source=connection.getpeername()[0]
  if source!=expected_source: raise RuntimeError('unexpected peer source: '+source)
  with connection.makefile('rb') as stream: received=stream.read(len(token)+1)
  if received!=token.encode(): raise RuntimeError('unexpected peer payload')
  connection.sendall(received)
 print(json.dumps({'source':source,'destination':address,'bytes':len(received)}),flush=True)
'''
CLIENT = '''import socket,sys
source,destination,port,token=sys.argv[1:]
with socket.socket(socket.AF_INET6 if ':' in source else socket.AF_INET) as connection:
 connection.settimeout(10)
 connection.bind((source,0))
 connection.connect((destination,int(port)))
 connection.sendall(token.encode())
 connection.shutdown(socket.SHUT_WR)
 with connection.makefile('rb') as stream: received=stream.read(len(token)+1)
 if received!=token.encode(): raise RuntimeError('unexpected peer response')
'''


def ssh_command(args, guest, code, *values):
    return [sys.executable, str(Path(__file__).resolve().parents[1] / 'ssh-guest.py'),
            '--context', args.context, '--namespace', args.namespace,
            '--release', guest['release'], '--address', guest['address'],
            '--identity', str(args.identity), '--known-hosts', str(guest['known_hosts']),
            '--user', args.user, '--', shlex.join(['python3', '-u', '-c', code, *map(str, values)])]


@contextmanager
def guest_process(command):
    # The SSH helper starts ssh, which starts kubectl. Own the whole local
    # process group so a failed probe cannot leave relays waiting behind it.
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               text=True, start_new_session=True)
    try:
        yield process
    finally:
        # Reap an exited leader before signaling: macOS may return EPERM for
        # a process group containing only its unreaped zombie.
        process.poll()
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
        process.stdout.close()


def listener_port(process, timeout=30):
    deadline = time.monotonic() + timeout
    line = bytearray()
    while len(line) < 1024:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([process.stdout], [], [], remaining)[0]:
            raise RuntimeError('timed out waiting for guest listener')
        value = os.read(process.stdout.fileno(), 1)
        if not value:
            raise RuntimeError('guest listener exited before reporting a port')
        if value == b'\n':
            port = json.loads(line)['port']
            if type(port) is not int or not 1 <= port <= 65535:
                raise RuntimeError('guest listener returned an invalid port')
            return port
        line.extend(value)
    raise RuntimeError('guest listener returned an oversized startup message')


def probe(args, source, destination):
    token = secrets.token_hex(16)
    command = ssh_command(args, destination, SERVER, destination['address'], source['address'], token)
    with guest_process(command) as server:
        port = listener_port(server)
        client_command = ssh_command(args, source, CLIENT, source['address'], destination['address'], port, token)
        with guest_process(client_command) as client:
            client.communicate(timeout=30)
            if client.returncode:
                raise RuntimeError('guest sender failed')
        output, _ = server.communicate(timeout=15)
        if server.returncode:
            raise RuntimeError('guest listener failed')
        result = json.loads(output)
        expected = {'source': source['address'], 'destination': destination['address'], 'bytes': len(token)}
        if result != expected:
            raise RuntimeError('guest listener did not confirm the expected source and payload')
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--context', required=True)
    parser.add_argument('--namespace', default='bmc-lab')
    parser.add_argument('--identity', required=True, type=Path)
    parser.add_argument('--user', default='lab')
    for name, release in [('first', 'target-one'), ('second', 'target-two')]:
        parser.add_argument('--' + name + '-release', default=release)
        parser.add_argument('--' + name + '-address', required=True, type=ipaddress.ip_address)
        parser.add_argument('--' + name + '-known-hosts', required=True, type=Path)
    args = parser.parse_args()
    if not args.context.startswith('kind-'):
        parser.error('select an explicit kind context')
    if args.first_address == args.second_address or args.first_release == args.second_release:
        parser.error('select two distinct guest addresses and target releases')
    if args.first_address.version != args.second_address.version:
        parser.error('guest addresses must use the same IP family')
    guests = [{'address': str(getattr(args, name + '_address')),
               'release': getattr(args, name + '_release'),
               'known_hosts': getattr(args, name + '_known_hosts')} for name in ('first', 'second')]
    try:
        results = [probe(args, guests[0], guests[1]), probe(args, guests[1], guests[0])]
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        print('connectivity check failed: ' + str(error), file=sys.stderr)
        raise SystemExit(1)
    print(json.dumps({'directions': results}, indent=2))


if __name__ == '__main__':
    main()
