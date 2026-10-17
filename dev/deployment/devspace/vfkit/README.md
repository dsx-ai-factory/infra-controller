# vfkit and isolated Linux labs

Use [vfkit](https://github.com/crc-org/vfkit) to run Linux VMs on Apple Silicon,
and the bundled lab helpers to connect them to an isolated IPv4/IPv6 network.
Each lab has a private Ethernet switch and a diskless Alpine gateway providing
DHCPv4, stateful DHCPv6, DNS, router advertisements, IPv4 NAT and NAT66.

The gateway has two NICs: a WAN connected through Apple NAT or a physical
interface, and a LAN connected to the private switch. Additional VMs attach only
to that LAN. A bridged **gateway WAN** does not bridge lab guests onto the
physical network.

## Prerequisites

- Apple Silicon macOS; the lab launcher requires macOS 13 or newer and vfkit
  0.6.2 or newer.
- Python 3, vfkit, curl, OpenSSH, xorriso, OpenSSL and GnuPG on the host.
- Internet access for initial image/package downloads and the gateway's
  IPv4/IPv6 HTTPS readiness checks.
- For bridged WAN, install
  [vmnet-helper](https://github.com/nirs/vmnet-helper#installation).
  The launcher finds `vmnet-run` on `PATH`,
  `/opt/homebrew/opt/vmnet-helper/libexec/`, or `/opt/vmnet-helper/bin/`.

Run the examples **from this directory**, in the same shell:

```bash
command -v vfkit python3 curl ssh ssh-keygen xorriso openssl gpg gpgv
vfkit --version
python3 vm.py --help
lab_root="$HOME/.vfkit-labs"
```

Use an initially empty state directory outside the checkout. The examples
always supply `--lab-root`; do not point it at an existing standalone vflab
state directory. Keep the checkout available while its helpers are running.

## Start a lab

### Bridged WAN

Choose an authorized physical interface with upstream IPv4 and IPv6. Replace
`en0` with the intended interface:

```bash
python3 vm.py --lab-root "$lab_root" --lab-id 1 \
  --lab-uplink bridged --lab-interface en0 lab-up
python3 vm.py --lab-root "$lab_root" --lab-id 1 lab-status
```

Only the gateway joins the physical network. Its key-only SSH listener is
exposed on its WAN addresses. Bridged traffic may bypass the host VPN and
network admission policy may reject the extra MAC or DHCP lease. There is no
automatic interface selection or fallback to NAT.

### NAT WAN

To create a separate lab using Apple's built-in NAT:

```bash
python3 vm.py --lab-root "$lab_root" --lab-id 2 --lab-uplink nat lab-up
```

Omit `--lab-interface` for NAT. NAT is the default for a newly created lab when
`--lab-uplink` is omitted. Working upstream IPv6 is still required: NAT66 cannot
supply connectivity that the host network lacks.

On first use, `lab-up` builds shared gateway assets using a temporary diskless
package-builder VM. It then creates the selected switch and gateway and checks
IPv4 and IPv6 HTTPS to `https://www.cloudflare.com/cdn-cgi/trace`. Failure leaves
state available for diagnosis and returns an error; blocking that endpoint also
fails readiness. The gateway uses 2 vCPUs and 1024 MiB RAM; the package-builder
uses the same resources. Additional guests have separate resource allocations.

## Lab configuration and addressing

All options precede the action. Lab actions accept `--lab-root` and
`--lab-id`, not `--vm-dir`. IDs are decimal integers `1..99` without leading
zeroes, defaulting to `1`. If `--lab-root` is omitted, the launcher's default is
`$HOME/.nico-devspace/vfkit-labs`; supplying it explicitly keeps these examples
in their own state directory. Commas and newlines in paths are unsupported.

The root is resolved to its canonical path and must be empty or marked as owned
by this launcher. Symlinked `labs/` and per-lab state directories are rejected.
The owner marker prevents adoption of arbitrary directories. A new lab saves
its WAN mode, interface and generated MACs.
Later `lab-up` calls reuse these settings; conflicting values are rejected even
after stopping the lab. Use another ID or delete the unused lab before choosing
a different configuration. Only `lab-up` accepts uplink/interface options.

| Setting | Lab X |
| --- | --- |
| IPv4 gateway | `10.X.0.1/24` |
| IPv4 DHCP pool | `10.X.0.10`–`10.X.0.250` |
| IPv6 gateway | `fd00:X::1/64` |
| IPv6 DHCP pool | `fd00:X::10`–`fd00:X::ff` |
| Gateway persistence | `$lab_root/labs/X/data/` |

The decimal spelling is used as the IPv6 hextet: lab 12 uses `fd00:12::/64`.
Guests need **DHCPv6 and router-advertisement support**, not just SLAAC.
Choose prefixes that do not overlap the upstream LAN or guest workloads;
startup rejects a WAN IPv4 lease inside the lab's own subnet.

Each lab has a separate switch. Forwarding to `10.0.0.0/8` and `fd00::/8` is
blocked to prevent routed cross-lab access; this also blocks corporate/VPN
destinations in those ranges. This is development network isolation, not a
security boundary against the host administrator or trusted guests.

This launcher selects a dual-stack LAN. Standalone vflab's `alpine-gw` CLI,
managed client commands, `shared` WAN mode and IPv4-only/IPv6-only LAN selection
are not exposed here. The helpers under `lab/` are lifecycle implementation
details, not a second interchangeable command interface.

## Attach a Linux VM

First obtain the actual switch path from `lab-status`:

```bash
lab_socket=$(python3 vm.py --lab-root "$lab_root" --lab-id 1 lab-status \
  | python3 -c 'import json, sys; print(json.load(sys.stdin)["socket"])')
test -S "$lab_socket"
test -S "${lab_socket%.sock}.control"
```

The switch lives in a short, per-user runtime directory, not under
`$lab_root/labs/X/network/`. The companion `switch.control` socket transfers
a socketpair descriptor to each VM. Always use `lab/lib/socket_vm.py` for
this attachment: it avoids the transmit stalls seen with direct named-datagram
attachments and terminates its child if the switch disconnects.

The following is an **attachment template**, not an OS installer. Before
running it, put an EFI-bootable ARM64 **raw** disk at
`$HOME/.vfkit-guests/example/root.raw`. The guest must already be configured
for virtio networking, DHCPv4/DHCPv6/RA, and whatever console or SSH access you
need. No account, public key or guest network configuration is injected by this
command. Never attach the same writable disk to two running VMs.

```bash
guest_dir="$HOME/.vfkit-guests/example"
test -f "$guest_dir/root.raw"
python3 lab/lib/socket_vm.py -- vfkit \
  --cpus 4 --memory 16384 \
  --bootloader "efi,variable-store=$guest_dir/efi-vars,create" \
  --device "virtio-blk,path=$guest_dir/root.raw" \
  --device "virtio-net,unixSocketPath=$lab_socket,mac=02:00:00:01:00:10" \
  --device virtio-rng \
  --device "virtio-serial,logFilePath=$guest_dir/serial.log"
```

Here `--memory` is in **MiB**: 16384 means 16 GiB. The disk size is whatever
the supplied image provides; the command does not create or resize it.
`create` creates the EFI variable store only when missing. Supply a unique MAC,
disk and state paths per VM. Do not add a second NAT or bridged NIC if the guest
should use only the isolated network. See
[vfkit's device and bootloader reference](https://github.com/crc-org/vfkit/blob/main/doc/usage.md)
for other guest configurations.

The supervisor runs in the foreground. Shut down the guest from inside Linux
before closing its terminal. Ctrl-C or loss of the switch terminates the vfkit
process; neither is a substitute for an orderly guest shutdown. The lab's
lifecycle does not provision, resize or delete custom guest disks.

The private switch creates no macOS LAN interface, so a guest's lab address is
not automatically reachable from the host. Use a separately configured console
or reach guest SSH through the gateway as a jump host. The gateway accepts
root SSH at its WAN IPv4 lease, recorded in the lab's `serial.log`, using
`$lab_root/labs/X/id_ed25519`; keep its host-key records in that lab's
`known_hosts`. Guest SSH still requires the guest's own account and authorized
key. The gateway access key does not automatically grant access to other VMs.

## Status, stop and delete

```bash
python3 vm.py --lab-root "$lab_root" --lab-id 1 lab-status
# Shut down every guest attached to lab 1 before stopping the lab.
python3 vm.py --lab-root "$lab_root" --lab-id 1 lab-stop
# Restart using the saved WAN configuration.
python3 vm.py --lab-root "$lab_root" --lab-id 1 lab-up
```

`lab-status` prints JSON containing saved configuration, state directory, socket
path, gateway/switch process IDs and the number of running attachments. Null
process IDs mean those components are stopped; a successful status command is
not a connectivity test. `lab-up` starts stopped components or checks an
already-running gateway; it does not apply guest-script changes in place.

`lab-stop` refuses while guests remain attached, including custom socket-mode
clients. Otherwise it requests gateway poweroff over SSH and then stops the
switch, preserving state. Missing WAN IPv4 management access or shutdown timeout
is an error, not permission to force-delete the data. After a switch failure,
confirm affected guests have stopped before restarting the lab and guests.

To permanently delete an unused lab's gateway state:

```bash
python3 vm.py --lab-root "$lab_root" --lab-id 1 --yes lab-delete
```

`--yes` is required and is invalid for other actions. Deletion stops an unused
lab and irreversibly removes its gateway state, leases, access/host keys, ISO and
logs. It refuses running attachments and unexpected files. It does **not**
delete custom guest disks, other labs or the shared image cache.
There is no all-labs reset command here.

## Images and stored data

| Location | Contents |
| --- | --- |
| `$lab_root/images/downloads/` | Alpine source ISO and publisher signature. |
| `$lab_root/images/gateway-VERSION-aarch64/` | Shared kernel, initramfs, base ISO and offline packages. |
| `$lab_root/images/gateway-builder-state/` | Temporary package-builder logs and state. |
| `$lab_root/labs/X/` | Saved configuration, generated gateway ISO, SSH access key, known hosts and logs. |
| `$lab_root/labs/X/data/` | Persistent DHCP state and SSH host identity. |
| Runtime path reported by `lab-status` | Private switch/control sockets and process IDs. |

The gateway boots into RAM. Only its persisted state survives reboot; other
changes must be made to the source guest scripts and applied on the next
stop/start. Stop all gateways using shared boot assets before rebuilding them.
State directories contain private keys and should not be shared publicly.

The launcher pins Alpine 3.24.1. The builder checks the downloaded ISO's SHA256
and verifies its `.asc` signature with the bundled
[Alpine publisher key](lab/image/keys/alpine.asc), including on cached-source
reuse. Verification uses an isolated GnuPG home, not personal keyrings or
automatic key retrieval. Old caches missing the signature download it once;
invalid or missing signatures fail closed. A source ISO with its valid cached
signature can be verified offline, but `lab-up` still requires Internet HTTPS.

The trusted fingerprint is `0482D84022F52DF1C4E7CD43293ACD0907D9495A`, published
on [Alpine's downloads page](https://www.alpinelinux.org/downloads/).
Key rotation needs a reviewed update verified against the publisher's
fingerprint. Verifier/key changes invalidate derived gateway image caches.
Package repositories are not frozen, so complete gateway builds are not
bit-for-bit reproducible.

## Troubleshooting

- **Gateway not ready:** inspect `serial.log`, `gateway.log` and `switch.log`
  in the directory reported by `lab-status`. The console includes the WAN DHCP
  lease and gateway boot failures. Check upstream DHCP and interface policy.
- **IPv6 Internet fails:** separate local DHCPv6/RA success from upstream
  forwarding and DNS. NAT66 needs an actual upstream route. The gateway's
  `vflab-ipv6-router` service checks routes every 30 seconds and solicits fresh
  advertisements when no usable default has more than 120 seconds remaining.
  It does not fabricate routes, extend expired lifetimes or restart DHCP.
  Logs are under that service tag in the gateway's `/var/log/messages`.
- **Large transfers stall:** NAT uplinks use a 1280-byte MTU and advertise it to
  clients; bridged labs advertise 1500. Verify the guest honors DHCP/RA settings
  and uses the socketpair supervisor.
- **Companion socket missing:** restart the lab with the bundled switch, after
  stopping its clients. Do not attach to an older datagram-only switch.
- **Signature failure:** do not disable verification. Inspect the key and
  downloads; remove only a damaged source ISO and its `.asc` from the download
  cache before retrying.
- **Rebuild refused:** stop every gateway using the shared assets. Running
  gateways are not updated in place. Do not remove shared caches during a
  gateway run or an image build.

## Lab implementation and tests

| Component | Responsibility |
| --- | --- |
| [`vm.py`](vm.py) lab actions | CLI entrypoints for starting, inspecting, stopping and deleting labs. |
| [`lab_manager.py`](lab_manager.py) | State ownership, gateway configuration, process tracking and readiness. |
| [`lab/lib/switch.py`](lab/lib/switch.py) | Private Ethernet switch and socketpair descriptor handoff. |
| [`lab/lib/socket_vm.py`](lab/lib/socket_vm.py) | Attach one lab NIC and supervise the guest process. |
| [`lab/image/gateway-image.py`](lab/image/gateway-image.py) | Build shared offline assets and per-lab gateway ISOs. |
| [`lab/image/extract-kernel.py`](lab/image/extract-kernel.py) | Extract a directly bootable kernel for vfkit. |
| [`lab/guest/`](lab/guest/) | Diskless boot, package collection, DHCP/DNS/NAT and IPv6 route renewal. |
| [`lab/lib/image_verification.py`](lab/lib/image_verification.py) | Signature verification with a selected bundled key. |

Run the host-only regression suite from this directory:

```bash
python3 -B -m unittest discover -s . -p 'test_*.py'
```

It needs GnuPG and permission to create temporary Unix sockets. It does not boot
VMs or validate live upstream connectivity.
