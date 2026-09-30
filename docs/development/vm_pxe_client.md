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
