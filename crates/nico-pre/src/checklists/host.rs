/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! `nico-pre-host`: each site controller over Redfish from the staging host, before the control-plane DPU is provisioned.

use crate::harness::{Check, todo};

pub(crate) const CHECKS: &[Check] = &[
    Check {
        id: "host.hardware.prerequisites",
        expected: "sockets, cores, memory, NVMe and network adapters at or above the site-controller prerequisites",
        detail: "Reads: per site controller over Redfish on `bmc`: `Systems/<id>` \
                 ProcessorSummary and MemorySummary, `Processors`, `Storage` and `Drives`, \
                 `Chassis/<id>/NetworkAdapters` and `Ports`; the prerequisites page for the site \
                 controller role (2 sockets, 24+ cores per socket, 256 GiB memory, total NVMe 4 \
                 TiB or more with an OS disk and a data disk of 1 TiB or more, Secure Erase where \
                 reported); the adapters expected per `dpu-in-the-site-controllers`.\n\
                 Pass: every value at or above the minimum on every controller; observed `sc1: \
                 2x32 cores, 512 GiB, 4x1.92 TB NVMe, 2 adapters`.\n\
                 Warn: memory between 256 and 512 GiB (the recommended value); Secure Erase not \
                 reported.\n\
                 Fail: a value below the minimum; observed names the controller, the value and \
                 the minimum; fix: hardware.\n\
                 Skip: never.\n\
                 Test: bmc-mock profiles for the supported models; cases: the reference profile \
                 passes; one socket fails naming cores; 128 GiB fails memory; a single NVMe fails \
                 the layout; 384 GiB warns.",
        run: todo,
    },
    Check {
        id: "host.hardware.tpm",
        expected: "a TPM 2.0 present and enabled",
        detail: "Reads: per controller, `Systems/<id>` TrustedModules (or TrustedComponents) over \
                 Redfish.\n\
                 Pass: a module with InterfaceType TPM2_0 and Status Enabled on every controller; \
                 observed `3 controllers: TPM 2.0 enabled`.\n\
                 Fail: no module, TPM 1.2, or Status Disabled; observed names the controller and \
                 what was found; fix: enable it in BIOS setup.\n\
                 Skip: never.\n\
                 Test: bmc-mock profiles; cases: TPM 2.0 enabled passes; absent fails; disabled \
                 fails; 1.2 fails naming the version.",
        run: todo,
    },
    Check {
        id: "host.firmware.bmc",
        expected: "BMC firmware at the HCL pin for the model",
        detail: "Reads: per controller, `Managers/<id>` FirmwareVersion (or \
                 `UpdateService/FirmwareInventory`); `model`; the HCL pin table (a \
                 placeholder until the provider guide lands).\n\
                 Pass: the running version equals or exceeds the pin for `model`; observed `sc1 \
                 Dell R760: iDRAC 7.10.30.00 (pin 7.10.30.00)`.\n\
                 Warn: the pin table is the placeholder; a version above the pin.\n\
                 Fail: a version below the pin, or a model absent from the table; observed names \
                 the controller, the version and the pin; fix: update the BMC firmware.\n\
                 Skip: never.\n\
                 Test: bmc-mock profiles with FirmwareVersion set per case; cases: equal passes; \
                 above warns; below fails naming both; a model not in the table fails naming the \
                 model.",
        run: todo,
    },
    Check {
        id: "host.firmware.uefi",
        expected: "UEFI firmware at the HCL pin for the model",
        detail: "Reads: per controller, `Systems/<id>` BiosVersion; `model`; the HCL pin table.\n\
                 Pass: the version equals or exceeds the pin for `model`; observed as for \
                 `host.firmware.bmc`.\n\
                 Warn: as `host.firmware.bmc`.\n\
                 Fail: as `host.firmware.bmc`, fix: update the UEFI firmware.\n\
                 Skip: never.\n\
                 Test: as `host.firmware.bmc` with BiosVersion.",
        run: todo,
    },
    Check {
        id: "host.uefi.posture",
        expected: "UEFI boot, Secure Boot, IOMMU and virtualization enabled; SR-IOV when used",
        detail: "Reads: per controller, `Systems/<id>/Bios` Attributes over Redfish, through a \
                 per-vendor attribute map (boot mode, Secure Boot, IOMMU or VT-d or AMD IOMMU, \
                 virtualization, SR-IOV); `managed-host-kinds-in-the-fleet` for whether NIC \
                 virtual functions are used.\n\
                 Pass: boot mode UEFI, Secure Boot enabled, IOMMU on, virtualization on; SR-IOV \
                 on when the site uses NIC virtual functions; observed `3 controllers: all \
                 attributes as required`.\n\
                 Fail: an attribute off or unreadable; observed `sc2: SecureBoot Disabled, IOMMU \
                 Disabled`; fix: enable it in BIOS setup and reboot; attribute names differ per \
                 vendor.\n\
                 Skip: never.\n\
                 Test: bmc-mock profiles; cases: all on passes; Secure Boot off on one host fails \
                 naming it; SR-IOV off with VFs used fails; SR-IOV off without VFs passes; an \
                 unmapped vendor fails naming the missing map.",
        run: todo,
    },
    Check {
        id: "host.bmc.virtual-media",
        expected: "at least one CD/DVD slot that offers InsertMedia; two for the one-pass OS plus DPU install",
        detail: "Reads: per controller, `Managers/<id>/VirtualMedia` members with MediaTypes CD \
                 or DVD, and each member's `Actions` block for `#VirtualMedia.InsertMedia` (with \
                 its `@Redfish.ActionInfo` or allowable values when present). Inspection only: \
                 the check never invokes `InsertMedia`, since a BMC may attach the image before \
                 it validates the URL and leave the slot occupied.\n\
                 Pass: at least two CD or DVD slots whose `Actions` offer `InsertMedia`; observed \
                 `sc1: 2 CD slots offer InsertMedia`.\n\
                 Warn: exactly one slot; the one-pass install then runs in two passes; fix: free \
                 or add a slot.\n\
                 Fail: no slot, or no slot whose `Actions` offer `InsertMedia`; fix: enable virtual \
                 media in the BMC.\n\
                 Skip: never.\n\
                 Test: bmc-mock profiles; cases: two slots pass; one slot warns naming the two-pass \
                 install it blocks; no CD slot fails; a slot without the action fails.",
        run: todo,
    },
    Check {
        id: "host.dpu.mode",
        expected: "a BlueField-3 in DPU mode at or above the HCL pins on the DPU BMC",
        detail: "Reads: per controller, the DPU BMC at `dpuBmc` over Redfish: the system model, \
                 the mode (DPU or NIC) from the BMC's attributes, the BMC, UEFI and NIC firmware \
                 versions; the HCL pins; `fleet.dpu.generation`.\n\
                 Pass: a BlueField-3 in DPU mode with every firmware at or above its pin; \
                 observed `3 DPU BMCs: BF3, DPU mode, firmware at pins`.\n\
                 Fail: NIC mode; a firmware below its pin; a BlueField-4, failed by name as \
                 unsupported on the control-plane path; observed per DPU; fix: `mlxconfig` to DPU \
                 mode or the firmware update.\n\
                 Skip: when `dpu-in-the-site-controllers` is `no DPU`.\n\
                 Test: bmc-mock DPU profiles; cases: BF3 in DPU mode passes; NIC mode fails; BF4 \
                 fails naming BF4; firmware below the pin fails naming both; `no DPU` skips.",
        run: todo,
    },
    Check {
        id: "host.dpu.ports",
        expected: "the DPU ports visible as adapters on the host BMC with link up",
        detail: "Reads: per controller, the host BMC's `Chassis/<id>/NetworkAdapters` and `Ports` \
                 for the BlueField (by manufacturer or model), with LinkStatus and \
                 CurrentSpeedGbps where reported.\n\
                 Pass: both BlueField ports present with link up; observed `sc1: 2 ports, LinkUp, \
                 200 Gbps`.\n\
                 Warn: a port with link down, or speed not reported.\n\
                 Fail: the BlueField not visible as an adapter; fix: seating or firmware.\n\
                 Skip: when `no DPU`.\n\
                 Test: bmc-mock profiles; cases: both up passes; one down warns naming the port; \
                 adapter missing fails; `no DPU` skips.",
        run: todo,
    },
    Check {
        id: "host.power.control",
        expected: "the BMC answers a power-state query and advertises the reset types NICo uses",
        detail: "Reads: per controller, `Systems/<id>` PowerState and the `ComputerSystem.Reset` \
                 action's `ResetType@Redfish.AllowableValues`.\n\
                 Pass: PowerState answers and the allowable values include On, ForceOff, \
                 GracefulShutdown and ForceRestart (the ones provisioning and NICo use); observed \
                 `3 controllers: power On; reset types complete`.\n\
                 Fail: PowerState unreadable, or a required reset type absent; observed names the \
                 missing type; fix: BMC firmware or licence.\n\
                 Skip: never.\n\
                 Test: bmc-mock profiles; cases: the full set passes; `ForceRestart` absent fails \
                 naming it; PowerState unreadable fails.",
        run: todo,
    },
    Check {
        id: "host.count.answering",
        expected: "every declared site controller answers over Redfish",
        detail: "Reads: `hosts.siteControllers`; one `GET /redfish/v1` per `bmc`.\n\
                 Pass: every declared controller answers; observed `3 of 3 answer`.\n\
                 Fail: fewer answering; observed names the ones that did not (`sc3 10.10.248.51: \
                 timeout`); fix: the path or the power.\n\
                 Skip: never. The other host checks run on the controllers that answer.\n\
                 Test: bmc-mock with one instance stopped; cases: all answer passes; one down \
                 fails naming it.",
        run: todo,
    },
    Check {
        id: "host.bmc.ntp",
        expected: "the BMC's NTP client points at the site's NTP servers",
        detail: "Reads: per controller, `Managers/<id>/NetworkProtocol` NTP (ProtocolEnabled, \
                 NTPServers); `network.ntpServers`.\n\
                 Pass: NTP enabled with the server set equal to `ntpServers`, order ignored; \
                 observed `3 controllers: NTP 10.10.0.21, 10.10.0.22`.\n\
                 Warn: a superset of the site's servers.\n\
                 Fail: NTP disabled, or a different server; observed names the controller and \
                 both lists; fix: set it in the BMC.\n\
                 Skip: never.\n\
                 Test: bmc-mock profiles; cases: equal passes; disabled fails; one extra server \
                 warns; a different server fails naming both lists.",
        run: todo,
    },
    Check {
        id: "host.nic.links",
        expected: "without a DPU, over SSH once the OS is installed: the NIC links up with the management and Kubernetes VLANs present",
        detail: "Reads: without a DPU, over SSH on each controller once the OS is installed: `ip \
                 -j link` and `ip -j addr`; the management and Kubernetes VLAN ids from the TOML; \
                 `hosts.stagingHost.sshUser`.\n\
                 Pass: the NIC's links up with both VLAN interfaces present and addressed; \
                 observed `sc1: 2 links up, vlan 100 and 200 present`.\n\
                 Fail: a link down, or a VLAN interface absent; observed per host; fix: netplan \
                 or the switch port.\n\
                 Skip: when `dpu-in-the-site-controllers` is a DPU; when the host checklist's \
                 Redfish rows report no OS installed (a refused SSH alone is a failure, not \
                 evidence of an absent OS).\n\
                 Test: fixture `ip -j` outputs fed to the parser; cases: both up passes; one link \
                 down fails; a VLAN missing fails naming it; a DPU skips.",
        run: todo,
    },
];
