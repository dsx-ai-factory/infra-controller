# Running a PXE Client in a VM

To test the PXE and DHCP boot process using a generic QEMU virtual machine, you start qemu
w/o graphics support. If the OS is graphical (e.g. ubuntu livecd) remove
`-nographic` and `display none` to have a GUI window start on desktop.

## Bridge Configuration

To allow the QEMU VM to join the bridge network that is used
for development, create or edit the file '/etc/qemu/bridge.conf' such that its contents are:

```text
$ cat /etc/qemu/bridge.conf
allow nico0
```

## TPM setup

A TPM (Trusted Platform Module) is a chip that can securely store artifacts used to authenticate the server. We have to pretend to have one.

### Install Software TPM emulator

- On Debian/Ubuntu:

  ```text
  sudo apt-get install -y swtpm swtpm-tools
  ```

### Create a directory for emulated TPM state

```text
mkdir /tmp/emulated_tpm
```

### Create initial configuration for the Software TPM

This step makes sure the emulated TPM has certificates.

```text
swtpm_setup --tpmstate /tmp/emulated_tpm --tpm2 --create-ek-cert --create-platform-cert
```

If you get an error in this step, try the following steps:

- Run `/usr/share/swtpm/swtpm-create-user-config-files`. Potentially with `--overwrite`.
  This writes the following files:
  - `~/.config/swtpm_setup.conf`
  - `~/.config/swtpm-localca.conf`
  - `~/.config/swtpm-localca.options`
- Check the content of the file `~/.config/swtpm_setup.conf`.
  If `create_certs_tools` has `@DATAROOT@` in its name, you have run into the
  bug [https://bugs.launchpad.net/ubuntu/+source/swtpm/+bug/1989598](https://bugs.launchpad.net/ubuntu/+source/swtpm/+bug/1989598) and [https://github.com/stefanberger/swtpm/issues/749](https://github.com/stefanberger/swtpm/issues/749).
  To fix the bug, edit `/usr/share/swtpm/swtpm-create-user-config-files`, search for
  the place where `create_certs_tool` is written, and replace it with the correct path
  to the tool. E.g.

  ```text
  create_certs_tool = /usr/lib/x86_64-linux-gnu/swtpm/swtpm-localca
  ```

  Then run `/usr/share/swtpm/swtpm-create-user-config-files` again.

### Start the TPM emulator

Run the following command in separate terminal to start a software TPM emulation

```text
swtpm socket --tpmstate dir=/tmp/emulated_tpm --ctrl type=unixio,path=/tmp/emulated_tpm/swtpm-sock --log level=20 --tpm2
```

Note that the process will automatically end if a VM that connects to this socket
is restarted. You need to restart the tool if you are restarting the VM.

## Start the services and seed the database

- `docker-compose up`
- `cargo make bootstrap-nico-docker`

If you see "No network segment defined for relay address: 172.20.0.11" in the nico-dhcp output, you forgot to run `cargo make bootstrap-nico-docker`.

## Start the VM

Make sure you have libvirt installed.

- Create it (once): `virsh define dev/libvirt_host.xml` (to rebuild first `virsh undefine --nvram ManagedHost`).
- Start it: `virsh start ManagedHost`.
- Look at the console (not in tmux!): `virsh console ManagedHost`.
- Stop it `virsh destroy ManagedHost`.

You can also use graphical interface `virt-manager`.

The virtual machine should fail to PXE boot from IPv4 (but gets an IP address) and IPv6, and then succeed from "HTTP boot IPv4", getting both an IP address and a boot image.

This should boot you into the pre-exec image. The user is `root` and password
is specified in the [mkosi.default](https://github.com/dsx-ai-factory/infra-controller/tree/main/pxe) file.

In order to exit out of console use `ctrl-a x`

virsh is part of libvirt. Libvirt is a user-friendly layer on top of QEMU (see next section to use it directly). QEMU is a hypervisor, it runs the virtual machine. QEMU uses kernel module KVM, which uses the CPU's virtualization instructions (Intel-VT or AMD-V).

## Start the VM (older, manual)

Do **not** do this step in `tmux` or `screen`. The QEMU escape sequence is Ctrl-a.

With TPM:

```text
sudo qemu-system-x86_64 -boot n -nographic -display none \
  -serial mon:stdio -cpu host \
  -accel kvm -device virtio-serial-pci \
  -netdev bridge,id=nicovm,br=nico0 \
  -device virtio-net-pci,netdev=nicovm \
  -bios /usr/share/ovmf/OVMF.fd -m 4096 \
  -chardev socket,id=chrtpm,path=/tmp/emulated_tpm/swtpm-sock \
  -tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-tis,tpmdev=tpm0
```

Without TPM:

```text
sudo qemu-system-x86_64 -boot n -nographic -display none \
  -serial mon:stdio -cpu host \
  -accel kvm -device virtio-serial-pci \
  -netdev bridge,id=nicovm,br=nico0 \
  -device virtio-net-pci,netdev=nicovm \
  -bios /usr/share/ovmf/OVMF.fd -m 4096
```

On Fedora change the `-bios` line to `-bios /usr/share/OVMF/OVMF_CODE.fd`.

**Note**: There is a known issue where iPXE DHCP fails on the first run. If this occurs, type `reset` in the EFI Shell to restart the PXE process; it will complete successfully on the second attempt.

**Note:** Verify the OVMF path for your system — depending on how the `ovmf` package was installed, the file may be under a subdirectory called `x64`.

**Note:** Known older issue on first boot that you'll land on a UEFI shell, have to `exit` back into the BIOS and select "Continue" in order to proceed into normal login.

## Verify Scout Boot NIC Selection

Scout boots in two stages. The small loader image, `scout.efi`, downloads
`scout.squashfs`, which contains the full Scout filesystem. The loader then
uses `systemctl soft-reboot` to start that filesystem with the same kernel.
The interface selected by the kernel's `mac=` argument must remain usable
through both stages.

This procedure covers the boot-MAC policy implemented in
[Apply Scout's Boot MAC Before Networking Starts](https://github.com/dsx-ai-factory/infra-controller/pull/7671)
for [Apply Scout Boot-MAC Policy Before Networking Starts](https://github.com/dsx-ai-factory/infra-controller/issues/7583).
It describes acceptance requirements, not a record of completed testing.
A helper unit test does not establish that the installed systemd units run
in the right order or that networking survives the switch between images.

### Prepare the Test

Use a disposable VM and an isolated test PXE service. You need permission to
change its boot artifacts and overrides, restart the VM, and disconnect its NICs.
Retain console access that does not depend on either test NIC.

Prepare the following inputs:

- Actual loader and full Scout artifacts from the build under test, with the
  architecture, source revision, and build identifier recorded separately.
- A copy of the [libvirt VM definition](https://github.com/dsx-ai-factory/infra-controller/blob/main/dev/libvirt_host.xml), extended
  to two NICs by duplicating its `interface` block. Give each block a distinct
  `mac` address and `target dev`. Set each `source bridge` to the lab bridge
  for that link. The template uses `carbide0`; the manual examples above use
  `nico0`. Use the bridges that your test environment actually provides.
  Their guest interface names must match `enx*`, `enp*`, or `enP*`, as Scout's
  default network policy requires. Both links must offer DHCP and reach the
  image server. Confirm the names and connectivity in the control boot.
- A lab-provided IPv6 router-advertisement source on each link for IPv6
  acceptance, with control over advertisements and address assignment.
- A known LLDP neighbor on each link. Record its chassis and port identifiers
  so received neighbors can be distinguished from stale observations. The lab
  must deliver those advertisements to each guest NIC; bridge connectivity
  alone does not establish LLDP reception.
- Serial logging and packet capture on both VM links from the start of boot.
  Keep the captures separate so the sending interface is unambiguous.

Record the exact image hashes before serving each pair:

```bash
sha256sum scout.efi scout.squashfs
```

Record the complete VM configuration and boot command line with those hashes.
Serve those exact files from the isolated image server. Retain the served URL
to file mapping and access records so the downloaded pair can be correlated
with the hashes. For a CPIO full image, hash the actual compressed archive
instead of `scout.squashfs`.

Use the existing
[boot-override set procedure](../manuals/nico-admin-cli/commands/boot-override/boot-override-set.md)
to control the VM's iPXE script:

1. Save the normal Scout iPXE script returned to the VM and any existing boot
   override, including user data. Identify the resolved machine interface UUID
   in that script; do not substitute a host machine ID or infer it from boot order.
2. Copy the complete script into the custom PXE file. Preserve `machine_id`,
   `server_uri`, `pxe_uri`, the `ds=nocloud` datasource, console settings, and
   the remaining arguments. Here, `machine_id` contains the interface UUID.
3. Set the loader URL to the served `scout.efi`. Add one `newrootfs=` value
   for the full image under test. Replace the existing `mac=` with the second
   NIC's six colon-separated hexadecimal octets. Keep the first NIC connected
   and DHCP-capable. For the control boot, remove `mac=`; for negative cases,
   replace it with the exact malformed or repeated input being tested.
4. Apply the custom PXE file to that resolved interface UUID, preserving any
   existing user-data override. Record the script and confirm the actual
   `/proc/cmdline` after boot. Appending another `mac=` to the normal script
   tests repeated input, not a new selection.
5. After testing, restore the saved override. If none existed, use
   [boot-override clear](../manuals/nico-admin-cli/commands/boot-override/boot-override-clear.md)
   for the same interface UUID.

Start the VM through its PXE path as described in [Start the VM](#start-the-vm).
The loader accepts `newrootfs=<full-image-url>` in the kernel command line.
Omission uses
`http://carbide-static-pxe.forge/public/blobs/internal/<architecture>/scout.squashfs`,
where `<architecture>` is the kernel's machine architecture. If you use this
default, test DNS must resolve that name to the isolated image server.
URLs ending in `.cpio`, `.cpio.zst`, or `.cpio.zstd`, optionally followed by
a query string, select CPIO extraction. These archives must be zstd-compressed
regardless of their suffix; other URLs select SquashFS. Supply only one
`newrootfs=` value. Repeated values use the last one; an empty value causes
failed download attempts.
`newrootfs=none` keeps the loader running for a separate diagnostic boot; it
does not exercise the switch to full Scout.

### Check Both Boot Stages

Start packet and serial capture before boot, then perform these checks:

1. Confirm both NICs receive DHCP in a control boot with `mac=` omitted.
   Confirm that the IPv6 advertisements used for testing create an address or
   route without restrictions, and that full Scout receives LLDP on both links.
2. Boot the selected-MAC case. Record the loader's selection and image
   download on the selected NIC. Let the loader perform its own soft reboot;
   do not replace this transition with a new VM boot.
3. After full Scout starts, collect the
   [Scout network diagnostics](../playbooks/stuck_objects/network_connectivity.md#scout-boot-nic-selection).
   Check the effective units and policy files as well as addresses and routes.
4. Match the selected interface to the supplied MAC in both stages. Require
   a usable global address and selected-interface egress to the image server.
   When IPv6 is offered, check IPv6 addresses and routing separately.
5. Verify that the other data NIC has no global addresses, global IPv4 routes,
   or non-link-local IPv6 unicast routes in the main routing table. The
   diagnostics include all tables; local and multicast entries are not failures.
   Correlate DHCP traffic with the boot stages. Firmware and iPXE DHCP before
   Linux starts is separate from Scout's policy. With a usable selection and
   the new policy active, DHCP must use the selected NIC. Reuse the control
   advertisements and confirm that they create no competing addresses or routes.
6. Verify fresh received LLDP neighbors on both links. Confirm that cloud-init
   still consumes the lab's metadata and user data after the image switch.
   Retain the delivered metadata and a known result of the test user data,
   such as a file it writes, alongside cloud-init's completion record.

Run the following commands in full Scout to identify the kernel boot and
received neighbors:

```bash
cat /proc/sys/kernel/random/boot_id
lldpcli -f json show neighbors
```

Where console access allows it, record the boot ID before and after the
switch by repeating the boot-ID read in the loader. LLDP collection runs in
full Scout, not in the loader. The boot ID remains unchanged across a soft
reboot. Retain the loader's download and selection records from its journal
or console capture before the switch. Do not assume a later journal contains
them. Correlate the loader's `LOADER: Cleaning up & switching` message with
soft-reboot completion and full-image units starting, including `forge-scout.service`.
An unchanged boot ID alone does not prove a switch.
LLDP transmission or carrier alone does not prove that neighbors were received.

### Check Recovery and Image Compatibility

Use a fresh boot for each input or image-pair change. While broad fallback
is active, verify that an HTTP request bound to the alternate NIC succeeds.
After selection returns, require egress on the selected NIC and withdrawal
of competing configuration. Record timestamps, helper results, unit state,
and networking before and after each transition.

For the duplicate-MAC case only, configure both guest NICs to report the same
MAC and supply one `mac=` argument with that address. Restore distinct MAC
addresses before running the other cases.

The recovery cases have distinct expected results:

| Case | Expected Result |
| --- | --- |
| Missing, repeated, or malformed `mac=` | Broad bootstrap remains available; no selected-MAC runtime policy survives. |
| Both guest NICs report the selected MAC on a fresh boot | Readiness triggers broad recovery immediately. After successful recovery, the later configuration call reports `result=skipped`, `reason=preferred_interface_count`, and `exit_status=0`. |
| Valid MAC with no matching NIC, no carrier, or no usable address | The bounded readiness check restores broad networking; the alternate NIC can obtain an address. |
| Selected link returns after fallback | A later configuration attempt restores selected-interface networking and removes competing addresses and routes. |
| Selected IPv6 address remains tentative or fails duplicate-address detection, with no other usable global address | It does not count as a usable address; readiness waits or falls back. |
| Loader kept running with `newrootfs=none` | Selection and unavailable-NIC recovery still run without downloading another image. |

For the IPv6 readiness case, suppress DHCPv4 and other usable IPv6 addresses
on the selected NIC. Use the lab's IPv6 controls to retain a tentative address
or introduce a duplicate on that link, then record the guest address flags.
A usable IPv4 address would satisfy readiness and invalidate this case.

For retry ownership, fallback results, and wait budgets, refer to
[Scout Boot NIC Selection](../playbooks/stuck_objects/network_connectivity.md#scout-boot-nic-selection).
Record Scout service restarts alongside the helper timestamps; each restart
can run the configuration check again. To exercise a later attempt explicitly
after restoring the selected link, run this command as root inside the
disposable guest:

```bash
/opt/forge/forge-scout-network.sh
```

This command changes network configuration. Capture its exit status and repeat
the network diagnostics afterward. An operational error must remain visible;
do not count a successful alternate-NIC transfer as proof of selected-NIC
recovery.

Repeat the selected-NIC and handoff checks with these image pairs. Here,
*new* includes [PR #7671](https://github.com/dsx-ai-factory/infra-controller/pull/7671), and *old* predates it. Record each source revision.
Choose images of the same architecture whose loader supports the full-image
format. The full root must supply modules compatible with the loader's kernel;
retain the kernel release and required-driver evidence for each pair.
A module mismatch invalidates the fixture, not the boot-MAC policy.

| Loader | Full Scout | Required Handoff Behavior |
| --- | --- | --- |
| New | New | Apply the selected-interface and packet checks to both stages. |
| Old | New | The old loader can configure other NICs before its later cleanup. Apply the early-policy checks to full Scout. To prove legacy cleanup, record the old loader's rule before the switch and its removal in the full image. |
| New | Old | Apply the early-policy checks to the loader. In the old full image, verify selected-interface egress and the legacy exclusion when selection was usable at handoff; otherwise verify broad networking. |

For the last pair, inspect the old root's
`/etc/systemd/network/00-forge-scout-nonpreferred.network`. With usable selection
at handoff, it must exist and its `Property=!INTERFACE=` match must exclude
the selected interface in the old root. Without usable selection at handoff,
the file must be absent. An exclusion matching every data NIC is a failure.
The old full image retains its own recovery behavior; these checks do not
establish recovery from a link lost after handoff. A modified unpacked
filesystem does not identify the artifacts actually shipped.

### Verify Core Discovery and Multi-DPU Hardware

Use a test Core deployment with permission to inspect its effective
configuration, DHCP ownership, and discovery records. Set and record
`allow_insecure_discovery = false`; the repository's Docker development
configuration explicitly sets it to `true`. With `false`, Core validates the
request's remote IP rather than trusting its supplied interface ID.

Use Core's DHCP and PXE services for this check, with the selected NIC's
lease recorded in Core. Retain the Core-observed source address, owning
machine interface, reported host identity, and successful discovery result.
Require that source address to equal the selected NIC's lease and that its
owning interface MAC equal `mac=`. Correlate the Core request records with
the stored interface and the guest diagnostics. Successful discovery through
the other NIC during fallback does not satisfy this check. Do not count
malformed-input boots as discovery acceptance or describe IP validation alone
as client-certificate authentication.

QEMU user-mode networking translates guest addresses. It cannot establish
Core's remote-IP ownership check against the guest's private DHCP address.
Likewise, starting an extracted loader kernel directly bypasses UEFI and
iPXE. Record these limits when using either technique for Linux networking
tests.

To validate firmware and DPU behavior, run separate physical acceptance on
a designated multi-DPU machine with authorization to change its served
artifacts, reboot it, and disconnect links. Retain independent console or
BMC access. Keep the same artifact, command-line, address, route, and discovery
evidence as for the VM, plus firmware boot order, selected MAC and PCI device,
each DPU's link and LLDP observations, and captures across the real image switch.
Repeat the unavailable-link recovery and secure discovery checks there.
Record architecture-specific results separately; an x86 VM run does not
establish Arm, BlueField driver, or firmware behavior.
