#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Destructive smoke test for a fresh, isolated nico-bmc-target release."""

import argparse
import json
import os
from pathlib import Path
import secrets
import re
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

# Credentials of the generated generic_ami test profile, not a hardware BMC.
FACTORY = {'username': 'root', 'password': 'factory_password'}
REQUEST = '''import base64,json,ssl,sys,urllib.request
from pathlib import Path
request=json.load(sys.stdin)
config=json.loads(Path('/data/inventory.json').read_text())
body=json.dumps(request['body']).encode() if request['body'] is not None else None
headers={'Authorization':'Basic '+base64.b64encode((request['username']+':'+request['password']).encode()).decode(),'Content-Type':'application/json'}
authority='['+config['bmcAddress']+']' if ':' in config['bmcAddress'] else config['bmcAddress']
req=urllib.request.Request('https://'+authority+request['path'],data=body,headers=headers,method=request['method'])
with urllib.request.urlopen(req,context=ssl._create_unverified_context(),timeout=15) as response:
 print(response.read().decode() or '{}')
'''
HUGEPAGES = '''import json
from pathlib import Path
config=json.loads(Path('/data/inventory.json').read_text())
pid=int(Path('/run/libvirt/qemu/target.pid').read_text())
status=Path(f'/proc/{pid}/status').read_text().splitlines()
actual=next(int(line.split()[1]) for line in status if line.startswith('HugetlbPages:'))
expected=config['memoryMiB']*1024
if actual != expected:
 raise ValueError(f'QEMU HugeTLB backing is {actual} KiB, expected {expected} KiB')
print(f'PASS QEMU holds {actual} KiB of explicit HugeTLB memory')
'''
CAPTURE = '''import json,socket,time,sys
from pathlib import Path
config=json.loads(Path('/data/inventory.json').read_text())
expected=bytes.fromhex(config['mac'].replace(':',''))
lease=sys.argv[1] if len(sys.argv)>1 else ''
with socket.socket(socket.AF_PACKET,socket.SOCK_RAW,socket.htons(3)) as capture:
 capture.bind((config['bridge'],0)); capture.settimeout(90)
 print('listening',flush=True)
 deadline=time.monotonic()+90
 while time.monotonic()<deadline:
  packet=capture.recv(65535)
  if len(packet)<300 or packet[12:14]!=b'\\x08\\x00' or packet[23]!=17: continue
  udp=14+(packet[14]&15)*4
  bootp=udp+8
  if packet[bootp+28:bootp+34]!=expected: continue
  if not lease:
   if packet[udp+2:udp+4]!=b'\\x00C' or packet[6:12]!=expected: continue
   print(json.dumps({'dhcp_mac':config['mac']}),flush=True); break
  if packet[udp+2:udp+4]!=b'\\x00D' or packet[bootp]!=2: continue
  if packet[bootp+236:bootp+240]!=b'\\x63\\x82\\x53\\x63': continue
  options=packet[bootp+240:]; message=None
  while options and options[0]!=255:
   if options[0]==0: options=options[1:]; continue
   if len(options)<2 or len(options)<2+options[1]: break
   if options[0]==53: message=options[2:2+options[1]]
   options=options[2+options[1]:]
  if message==b'\\x05' and socket.inet_ntoa(packet[bootp+16:bootp+20])==lease:
   print(json.dumps({'dhcp_mac':config['mac'],'lease':lease}),flush=True); break
 else: raise SystemExit('no DHCP frame from the configured guest MAC')
'''


def command(args, input='', timeout=60):
    result = subprocess.run(args, input=input, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    if result.returncode:
        # Do not include request input or credentials in diagnostic output.
        raise RuntimeError(f'command failed ({result.returncode}): {result.stderr[-1000:]}')
    return result.stdout.strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase', choices=['prepare', 'retained'])
    parser.add_argument('--context', required=True)
    parser.add_argument('--namespace', default='bmc-lab')
    parser.add_argument('--release', default='target-one')
    parser.add_argument('--credentials', required=True, type=Path,
                        help='private file created by prepare and reused by retained')
    parser.add_argument('--lease-address', default='', help='require a DHCP ACK for this IPv4 address instead of only a request')
    args = parser.parse_args()
    if args.lease_address:
        import ipaddress
        ipaddress.IPv4Address(args.lease_address)
    if not args.context.startswith('kind-'):
        raise ValueError('smoke test requires an explicitly selected kind context')
    kube = ['kubectl', '--context', args.context, '-n', args.namespace]
    deployment = json.loads(command(kube + ['get', 'deployment', args.release, '-o', 'json']))
    if deployment['metadata']['labels'].get('app.kubernetes.io/name') != 'nico-bmc-target':
        raise ValueError('selected deployment is not a native BMC test target')
    execute = kube + ['exec', '-i', 'deployment/' + args.release, '--']
    config = json.loads(command(execute + ['cat', '/data/inventory.json']))
    state = command(execute + ['virsh', '-c', 'qemu:///system', 'domstate', 'target'])
    if state != 'shut off':
        raise ValueError('smoke test must start with the selected guest powered off')

    def request(credentials, method, path, body=None):
        payload = dict(credentials, method=method, path=path, body=body)
        return json.loads(command(execute + ['python3', '-c', REQUEST], input=json.dumps(payload)))

    def snapshot():
        script = "from pathlib import Path; import json; print(json.dumps({p.name:p.stat().st_ino for p in [Path('/data/disks/root.qcow2'),Path('/data/disks/nvram.fd')]}))"
        return json.loads(command(execute + ['python3', '-c', script]))

    if args.phase == 'prepare':
        credentials = {'username': 'root', 'password': secrets.token_urlsafe(24)}
        record = {'credentials': credentials, 'config': config, 'inodes': snapshot()}
        fd = os.open(args.credentials, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'w') as stream:
            json.dump(record, stream)
        request(FACTORY, 'PATCH', '/redfish/v1/AccountService/Accounts/2', {'Password': credentials['password']})
        command(execute + ['qemu-io', '-f', 'qcow2', '-c', 'write -P 90 1048576 4096', '/data/disks/root.qcow2'])
    else:
        record = json.loads(args.credentials.read_text())
        credentials = record['credentials']
        if record['config'] != config or record['inodes'] != snapshot():
            raise ValueError('retained guest identity, disk or firmware file changed')
        command(execute + ['qemu-io', '-r', '-f', 'qcow2', '-c', 'read -P 90 1048576 4096', '/data/disks/root.qcow2'])
    systems = request(credentials, 'GET', '/redfish/v1/Systems')
    system_path = systems['Members'][0]['@odata.id']
    system = request(credentials, 'GET', system_path)
    if system['PowerState'] != 'Off':
        raise ValueError('BMC and libvirt power states disagree')
    domain = ET.fromstring(command(execute + ['virsh', '-c', 'qemu:///system', 'dumpxml', 'target']))
    guest_mac = domain.find('devices/interface/mac').attrib['address'].lower()
    guest_serial = domain.findtext("sysinfo/system/entry[@name='serial']")
    if system['SerialNumber'] != guest_serial or guest_serial != config['serial']:
        raise ValueError('Redfish serial and guest SMBIOS identity disagree')
    # GenericAmi advertises NIC MACs in BootOptions; its EthernetInterfaces is empty.
    options = request(credentials, 'GET', system['Boot']['BootOptions']['@odata.id'])
    advertised_macs = set()
    for member in options['Members']:
        option = request(credentials, 'GET', member['@odata.id'])
        advertised_macs.update(re.findall(r'(?:[0-9a-f]{2}:){5}[0-9a-f]{2}', option['DisplayName'].lower()))
    if guest_mac != config['mac'] or advertised_macs != {guest_mac}:
        raise ValueError('Redfish network boot MAC and guest NIC identity disagree')
    action = system['Actions']['#ComputerSystem.Reset']['target']
    capture = subprocess.Popen(execute + ['python3', '-u', '-c', CAPTURE, args.lease_address], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        import select
        if not select.select([capture.stdout], [], [], 15)[0] or capture.stdout.readline().strip() != 'listening':
            raise RuntimeError('guest DHCP capture failed to start')
        request(credentials, 'POST', action, {'ResetType': 'On'})
        # A saved firmware session can have finished PXE retries. Cold reboot it
        # through Redfish for a fresh packet-identity observation after restoration.
        if args.phase == 'retained':
            for _ in range(30):
                if request(credentials, 'GET', system_path)['PowerState'] == 'On':
                    break
                time.sleep(1)
            request(credentials, 'POST', action, {'ResetType': 'ForceRestart'})
        output, errors = capture.communicate(timeout=100)
        if capture.returncode or json.loads(output)['dhcp_mac'] != config['mac']:
            raise RuntimeError('guest did not send DHCP with its configured MAC: ' + errors[-500:])
    finally:
        if capture.poll() is None:
            capture.terminate()
            capture.wait(timeout=5)
    if request(credentials, 'GET', system_path)['PowerState'] != 'On':
        raise ValueError('guest firmware ran but BMC did not observe power-on')
    if config.get('hugepages'):
        print(command(execute + ['python3', '-c', HUGEPAGES]).strip())
    command(execute + ['test', '-s', '/data/logs/console.log'])
    if args.lease_address:
        print(f'PASS firmware DHCP ACK: {args.lease_address}')
    print(f'PASS {args.phase}: Redfish/guest identity, authenticated power, native firmware and DHCP identity {config["mac"]}; guest left running')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'target validation failed: {error}', file=sys.stderr)
        sys.exit(1)
