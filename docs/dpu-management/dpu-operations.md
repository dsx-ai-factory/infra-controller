# BlueField DPU Operations

This page covers common operator tasks for interacting directly with BlueField DPUs managed by NICo. For the automated DPU lifecycle (OS installation, firmware, health, reprovisioning), see [DPU Lifecycle Management](dpu-lifecycle-management.md).

## Connecting to a DPU

The DPU shares a physical 1GB ethernet connection for both BMC and OOB access. This interface has two different MAC addresses, so the OOB and BMC have unique IP addresses.

### Via DPU OOB (preferred)

If the OOB interfaces are provisioned, SSH directly to the DPU OOB IP using the DPU OOB credentials. This connects you to the DPU OS without going through the BMC.

```bash
ssh <username>@<dpu-oob-ip>
```

### Via BMC and rshim console

rshim must be enabled on the DPU before this method works. NICo enables rshim during ingestion as part of the `DpuDiscoveringState` flow. If it is not enabled, you can enable it manually from the BMC:

```bash
systemctl enable --now rshim
```

The BMC OS is a basic `busybox` shell with limited commands. SSH to the DPU BMC IP, then use `microcom` to access the DPU console:

```bash
ssh <username>@<dpu-bmc-ip>
microcom /dev/rshim0/console
```

Press Enter to bring up the login prompt. Use the DPU OOB credentials to log in. Press `Ctrl-X` to disconnect from the console.

## Restarting a DPU

From the BMC, you can restart the DPU without power-cycling the host:

```bash
echo "SW_RESET 1" > /dev/rshim0/misc
```

## Checking Firmware Versions

Use `mlxfwmanager` on the DPU to see the current firmware version and any pending version that will activate on the next power cycle:

```bash
mlxfwmanager
```

> **Note:** If a BFB install included firmware updates, they require a full host power cycle (not a reboot) to take effect.

## Checking Link Speed and Auto-Negotiation

To verify link speed and auto-negotiation on the DPU high-speed ports:

```bash
ethtool p0 | grep -P 'Speed|Auto'
ethtool p1 | grep -P 'Speed|Auto'
```

Expected output on a 25G port:

```
Speed: 25000Mb/s
Auto-negotiation: on
```

If auto-negotiation is not enabled, you can add udev rules to enable it automatically:

```bash
echo 'SUBSYSTEM=="net", ACTION=="add", NAME=="p0", RUN+="/sbin/ethtool -s p0 autoneg on"' >> /etc/udev/rules.d/83-net-speed.rules
echo 'SUBSYSTEM=="net", ACTION=="add", NAME=="p1", RUN+="/sbin/ethtool -s p1 autoneg on"' >> /etc/udev/rules.d/83-net-speed.rules
```
