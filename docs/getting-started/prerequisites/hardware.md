# Hardware Prerequisites

This page covers the hardware requirements for both the NICo site controller and the compute systems it manages.

## Site Controller

The site controller runs the NICo control plane on a Kubernetes cluster. A minimum of 3 nodes is required for high availability; 5 nodes are recommended for large sites (approximately 5,000 managed compute nodes). As a capacity-planning guideline, a 5-node cluster on recommended hardware can support ~5,000 managed compute nodes; this is a sizing guideline, not a hard limit — actual capacity varies by configuration, workload, and environment, so validate with SVT/scalability testing for your BOM.

| Component | Requirement |
|---|---|
| Server class | Any major OEM Gen5 server (e.g. Dell R760-class) |
| CPU | 2x modern x86_64 sockets (Intel Xeon / AMD EPYC), 24+ cores per socket |
| Memory | 256 GiB minimum, 512 GiB recommended |
| OS storage | 200-500 GiB NVMe SSD space (UEFI + Secure Boot) |
| K8s data storage | 1+ TiB NVMe SSD space dedicated to containerd, Kubelet, and logs |
| Networking | 1-2x 25/100 GbE ports (single-homed or dual-homed) |
| Out-of-band | BMC/iDRAC/iLO/XClarity (DHCP or statically addressed) |
| TPM | TPM 2.0 module present and enabled in BIOS/UEFI |
| Secure Erase | All local storage drives must support Secure Erase |

**Operating system**: Ubuntu 24.04 LTS, kernel 6.8+. Swap disabled (or minimal), NUMA enabled, virtualization/IOMMU enabled. Time sync: chrony or equivalent, synced to enterprise NTP.

**Storage layout**: Total local NVMe capacity should be 4 TiB or greater. Mount 1.7 TiB on `/` (root) on the NVMe OS disk (ext4 or xfs) — typical usage is 200–500 GiB. Mount `/var/lib/containerd` and `/var/lib/kubelet` on a separate NVMe data disk (1+ TiB, ext4/xfs, `noatime`). Consider a dedicated `/var/log` if there is heavy logging. Persistent app storage (SAN/NAS, Rook-Ceph) is not required for NICo itself.

### DPUs on Site Controller

DPUs are generally preferred in nodes hosting the NICo control plane components, but not strictly required. DPUs in these nodes are, however, the configuration that NICo QA regularly tests. If your site controller nodes are equipped with Bluefield-3 DPUs, ensure the following requirements are met:

- You have the correct DPU power cable from the server vendor.
- The Bluefield-3's operating mode is DPU mode.
- For BF3 DPUs, verify link speed and optics: BF3 runs at 200 Gb, so match ports to 200 Gb-capable optics, fiber, or DACs.
- Verify that the DPU can connect to the outside world (`curl -I https://www.nvidia.com`).
- The DPUs use the [tested BlueField-3 software versions](../../hcl.md#dpus).

### NICo Pod Network Reachability

NICo's control plane pods (DHCP, DNS, API, PXE, SSH console, and others) must be externally reachable from the networks they serve. Regardless of whether site controller nodes have DPUs, these pods must be routable to and from:

- DPU BMCs and Host BMCs of managed machines
- The admin network
- Tenant VPCs

The reference architecture is MetalLB advertising L3 LoadBalancer VIPs to the site controller's DPU uplinks (if DPUs are present) or directly to the ToR switches (if site controller nodes have no DPUs).

For better traffic isolation and routing, configure the LoadBalancer endpoints for NICo pods as route-tagged prefixes and import them into the admin and VPC routing profiles. See [VPC Routing Profiles](../../manuals/vpc/vpc_routing_profiles.md).

## Compute Systems (Managed Hosts)

Each managed host is a server paired with one or more NVIDIA BlueField DPUs. The DPU provides the primary data-plane connectivity and acts as the enforcement boundary for NICo.

| Component | Requirement |
|---|---|
| Server class | An [NVIDIA-certified system](https://docs.nvidia.com/ngc/ngc-deploy-on-premises/nvidia-certified-systems/index.html) with a data center classification |
| GPU | Refer to the [Hardware Compatibility List](../../hcl.md) for supported GPUs and systems. |
| DPU | One or more BlueField DPUs with 2x 200 Gb network interfaces and a 1 Gb BMC interface|
| Local storage | NVMe drives must support Secure Erase; firmware must be updated only via signed images; rollback must not be possible. |
| TPM | TPM 2.0 with Secure Boot support |
| UEFI | Must support preventing in-band host control. |
| Chassis BMC | Must support the following Redfish operations: power control, boot order, UEFI secure boot toggle, IPv6, firmware update, Serial-over-LAN |

The BlueField-3 B3220 P-Series DPU is suitable: 200GbE/NDR200 dual-port QSFP112 Network Adaptor (900-9D3B6-00CV-AA0). Other NICs on the host are automatically disabled during NICo installation.

NICo does not require any cabling or communication between the DPU and the host.

## Supported Hardware

For a list of tested host machines, DPUs, and validated firmware versions, refer to the [Hardware Compatibility List](../../hcl.md) page.

## BIOS/UEFI Settings

The following settings should be enabled on site controller and compute system BIOS/UEFI:

- **UEFI + Secure Boot** (with signed kernel/modules)
- **VT-x / AMD-V + IOMMU**
- **SR-IOV** (if using NIC VFs; otherwise leave off)
- **NTP** (locked to enterprise sources; clock drift alarms enabled)
