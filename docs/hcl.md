# Hardware Compatibility List

The Hardware Compatibility List (HCL) identifies hardware that is currently supported and compatible from a NICo perspective. It is not an exhaustive certification or qualification of each platform. For platform certification, qualification, and detailed hardware support information, contact the hardware manufacturer.

To add support for hardware that the NICo development team does not have access to, follow [Adding Support for New Hardware](development/new_hardware_support.md).

## Hosts

**Last Updated: August 31, 2026**

| Host Machine | BMC/Management Firmware Version | BIOS/UEFI Version | Misc. Firmware Version (FPGA, CPLD, LXPM, etc.) |
| ------------ | ------------------------------- | ----------------- | ----------------------------------------------- |
| ARS-121L-DNR | 01.08.02 / 01.03.16 (LCC) | 2.2a / 2.0 (LCC) | SAA Ver = 1.2.0-p6 / SUM = 2.14.0-p6 (LCC) |
| ARS-221GL-NR | 1.03.16 | 2.0 |  |
| Dell PowerEdge R640 | iDRAC 7.00.00.182 | 2.24.0 | 1.0.6 |
| Dell PowerEdge R750 | iDRAC 7.20.60.50 | 1.18.1 | 1.1.1 |
| Dell PowerEdge R760 | iDRAC 7.20.60.50 | 2.7.5 | 1.2.6 |
| Dell PowerEdge XE9680 | iDRAC 7.20.60.50 | 2.7.4 | 1.6.0 |
| Dell R670 | iDRAC 1.20.80.51 | 1.7.5 |  |
| Dell R760xd2 | iDRAC 7.20.80.50 | 2.9.4 | 1.1.2 |
| Dell R770 | iDRAC 1.20.80.51 | 1.7.5 |  |
| DGX H100 | 25.06.27 (DGXH100_H200_25.06.4 pkg) | 1.06.07 (DGXH100_H200_25.06.4 pkg) |  |
| DL380 Gen12 | 1.20.00 | 1.62_02-06-2026 |  |
| GB200 Compute Tray (1RU) | 1.3.2GA | 1.3.2GA | 1.3.2GA |
| GB200 NVL - Wiwynn | 25.06-2_NV_WW_02 | 1.3.2GA | 1.3.2GA |
| HPE ProLiant DL385 Gen10 Plus v2 | 3.15 | 3.80_09-05-2025 |  |
| Lenovo GB300 Compute Tray | 3.0.0 | 1.0.0GA | 1.0.0GA |
| Lenovo HS350X V3 | 1.20 | 2.17.0 |  |
| Lenovo SR650 V4 | 1.90 | 1.30 | 5.03.00 |
| Lenovo ThinkSystem SR650 | 10.40 | 4.30 | 2.13 |
| Lenovo ThinkSystem SR650 V2 | 5.70 | 3.60 | 3.31.01 |
| Lenovo ThinkSystem SR650 V2 OVX* | 5.70 | 3.60 | 3.31.01 |
| Lenovo ThinkSystem SR650 V3 | 6.92 | 3.70 | 4.21.01 |
| Lenovo ThinkSystem SR655 V3 | 5.80 | 5.70 | 4.20.03 |
| Lenovo ThinkSystem SR655 V3 OVX* | 5.80 | 5.70 | 4.20.03 |
| Lenovo ThinkSystem SR665 V3 OVX* | 5.80 | 5.70 | 4.20.03 |
| Lenovo ThinkSystem SR670 V2 | 6.10 | 3.30 | 3.31.01 |
| Lenovo ThinkSystem SR675 V3 | 14.10 | 8.30 | 4.20.03 |
| Lenovo ThinkSystem SR675 V3 OVX* | 14.10 | 8.30 | 4.20.03 |
| NVSwitch Tray DGX | 1.3.2GA | 1.3.2GA | 1.3.2GA |
| NVSwitch Tray - Wiwynn | 1.3.2GA | 1.3.2GA | 1.3.2GA |
| SSG-121E-NES24R | 01.04.19 | 2.7 | SAA Ver = 1.3.0-p1 |
| SYS-121H-TNR | X1.05.10 | 2.7 | SAA Ver = 1.3.0-p5 |
| SYS-221H-TN24R | X1.05.10 | 2.7 | SAA Ver = 1.3.0-p5 |
| SYS-221H-TNR | 1.03.18 | 2.7 | SAA Ver = 1.3.0-p7 |
| SYS-421GE-TNRT | 1.03.19 | 2.6 | SAA Ver = 1.2.0-p8 |
| SYS-821GE-TNHR | 1.03.18 | 2.7 | SAA Ver = 1.3.0-p7 |

\* OVX may not show up as an option; check the Server Serial Number to confirm.

### Hosts -- Under Development <Badge intent="info">Upcoming</Badge>

This list outlines platforms that are under development and have not undergone full unit testing.

| Host Machine | BMC/Management Firmware Version | BIOS/UEFI Version | Provisioning Manager Version |
| ------------ | ------------------------------- | ----------------- | ---------------------------- |
| N/A |  |  |  |

## DPUs

The following versions are the tested software pins, not automatically the
latest available releases. DOCA and HBN are versioned separately.

| DPU | DOCA | HBN |
| --- | --- | --- |
| BlueField 2 | 3.2.0 | Not specified |
| BlueField 3 | 3.2.2 | 3.2.2 |

## GPUs

NICo supports all current NVIDIA data center GPUs.

Unlike host platforms, GPU compatibility is not validated on a per-platform basis in this Hardware Compatibility List. GPU support is provided through the supported host platforms and NVIDIA software stack. For platform-specific GPU qualification, certification, thermal, power, or configuration guidance, refer to your server manufacturer.

| GPU Family | Support |
| ---------- | ------- |
| Current NVIDIA Data Center GPUs | Supported |
