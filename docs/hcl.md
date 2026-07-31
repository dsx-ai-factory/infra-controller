# Hardware Compatibility List

# Hardware Compatibility List

**Last updated July 24, 2026**

This Hardware Compatibility List (HCL) is provided for reference purposes only. Systems listed here have been unit tested or exercised internally in limited scenarios.
Inclusion in this list does not imply qualification, certification, or support, and does not represent a commitment to ongoing compatibility. For specific hardware support
inquiries or technical specifications, please contact the original hardware vendor.

The sections below distinguish platforms maintained by NVIDIA from platforms contributed and maintained by the community.

## NVIDIA-Maintained Platforms

### Hosts

| Host Machine                        | BMC/Management Firmware Version     | BIOS/UEFI Version   | Misc. Firmware Version (FPGA, CPLD, LXPM, etc.) |
|-------------------------------------|-------------------------------------|---------------------|-------------------------------------------------|
| GB200 NVL - Wiwynn                  | 25.06-2_NV_WW_02                    | 1.3.2GA             | 1.3.2GA                                         |
| NVSwitch Tray - Wiwynn              | 1.3.2GA                             | 1.3.2GA             | 1.3.2GA                                         |
| GB200 Compute Tray (1RU)            | 1.3.2GA                             | 1.3.2GA             | 1.3.2GA                                         |
| NVSwitch Tray DGX                   | 1.3.2GA                             | 1.3.2GA             | 1.3.2GA                                         |
| DGX H100                            | 25.06.27 (DGXH100_H200_25.06.4 pkg) | 1.06.07 (DGXH100_H200_25.06.4 pkg) |                                  |
| Lenovo ThinkSystem SR670 V2         | 6.10                                | 3.30                | 3.31.01                                         |
| Lenovo ThinkSystem SR675 V3         | 14.10                               | 8.30                | 4.20.03                                         |
| Lenovo ThinkSystem SR675 V3 OVX*    | 14.10                               | 8.30                | 4.20.03                                         |
| Lenovo ThinkSystem SR650            | 10.40                               | 4.30                | 2.13                                            |
| Lenovo ThinkSystem SR650 V3         | 6.92                                | 3.70                | 4.21.01                                         |
| Lenovo ThinkSystem SR650 V2         | 5.70                                | 3.60                | 3.31.01                                         |
| Lenovo ThinkSystem SR650 V2 OVX*    | 5.70                                | 3.60                | 3.31.01                                         |
| Lenovo ThinkSystem SR655 V3         | 5.80                                | 5.70                | 4.20.03                                         |
| Lenovo ThinkSystem SR655 V3 OVX*    | 5.80                                | 5.70                | 4.20.03                                         |
| Lenovo ThinkSystem SR665 V3 OVX*    | 5.80                                | 5.70                | 4.20.03                                         |
| Lenovo SR650 V4                     | 1.90                                | 1.30                | 5.03.00                                         |
| Lenovo HS350X V3                    | 1.20                                | 2.17.0              |                                                 |
| Dell PowerEdge XE9680               | iDRAC 7.20.60.50                    | 2.7.4               | 1.6.0                                           |
| Dell PowerEdge R750                 | iDRAC 7.20.60.50                    | 1.18.1              | 1.1.1                                           |
| SYS-221H-TNR                        | 1.03.18                             | 2.7                 | SAA Ver = 1.3.0-p7                              |
| Dell PowerEdge R760                 | iDRAC 7.20.60.50                    | 2.7.5               | 1.2.6                                           |
| ARS-121L-DNR                        | 01.08.02 / 01.03.16 (LCC)           | 2.2a / 2.0 (LCC)    | SAA Ver = 1.2.0-p6 / SUM = 2.14.0-p6 (LCC)      |
| SYS-221H-TN24R                      | X1.05.10                            | 2.7                 | SAA Ver = 1.3.0-p5                              |
| ARS-221GL-NR                        | 1.03.16                             | 2.0                 |                                                 |
| HPE ProLiant DL385 Gen10 Plus v2    | 3.15                                | 3.80_09-05-2025     |                                                 |
| DL380 Gen12                         | 1.20.00                             | 1.62_02-06-2026     |                                                 |
| SSG-121E-NES24R                     | 01.04.19                            | 2.7                 | SAA Ver = 1.3.0-p1                              |
| SYS-121H-TNR                        | X1.05.10                            | 2.7                 | SAA Ver = 1.3.0-p5                              |
| SYS-821GE-TNHR                      | 1.03.18                             | 2.7                 | SAA Ver = 1.3.0-p7                              |
| Dell R760xd2                        | iDRAC 7.20.80.50                    | 2.9.4               | 1.1.2                                           |
| Dell R670                           | iDRAC 1.20.80.51                    | 1.7.5               |                                                 |
| Dell R770                           | iDRAC 1.20.80.51                    | 1.7.5               |                                                 |
| SYS-421GE-TNRT                      | 1.03.19                             | 2.6                 | SAA Ver = 1.2.0-p8                              |
| Dell PowerEdge R640                 | iDRAC 7.00.00.182                   | 2.24.0              | 1.0.6                                           |

\* OVX may not show up as an option; check the Server Serial Number to confirm.

#### Hosts Under Development

This list outlines platforms that are under development and have not undergone full unit testing.

| Host Machine                        | BMC/Management Firmware Version     | BIOS/UEFI Version   | Provisioning Manager Version               |
|-------------------------------------|-------------------------------------|---------------------|--------------------------------------------|
| Lenovo GB300 Compute Tray           | 3.0.0                               | 1.0.0GA             | 1.0.0GA                                    |

### DPUs

| DPU          | Firmware / Software Version                       |
|--------------|---------------------------------------------------|
| BlueField 2  | DOCA 3.2.0                                        |
| BlueField 3  | DOCA 3.2.0                                        |

## Coming Soon: Community-Added Platforms

Community-added platforms will be listed here separately from NVIDIA-maintained platforms. Each entry will identify a contact for questions and issues and link to the corresponding GitHub contribution for implementation history and ongoing coordination.

No community-added platforms are currently listed.
## GPUs

NICo supports all current NVIDIA data center GPUs.

Unlike host platforms, GPU compatibility is not validated on a per-platform basis in this Hardware Compatibility List. GPU support is provided through the supported host platforms and NVIDIA software stack. For platform-specific GPU qualification, certification, thermal, power, or configuration guidance, refer to your server manufacturer.

| GPU Family | Support |
| ---------- | ------- |
| Current NVIDIA Data Center GPUs | Supported |

