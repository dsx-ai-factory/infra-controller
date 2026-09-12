// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package carbide holds the label, annotation, and CR-naming constants that
// form the contract between NICo and the DPF operator. The simulator must use
// these verbatim — they are mirrored from the NICo source of truth:
//
//	crates/machine-controller/src/dpf.rs   (labels)
//	crates/dpf/src/sdk.rs                   (annotations, CR-name derivation)
//
// If NICo changes any of these, this file must change with it.
package carbide

import "strings"

// Labels NICo sets on DPUDevice CRs. The dpu-machine-id label MUST be copied
// from the DPUDevice onto the DPU CR the simulator creates, or NICo cannot map
// a DPU event back to its machine (reverse lookup in dpf.rs).
const (
	LabelDPUMachineID    = "carbide.nvidia.com/dpu-machine-id"
	LabelControlledDev   = "carbide.nvidia.com/controlled.device" // "true"
	LabelHostBMCIP       = "carbide.nvidia.com/host-bmc-ip"
	LabelIsPrimaryDPU    = "carbide.nvidia.com/is-primary-dpu"
	LabelControlledNode2 = "carbide.nvidia.com/controlled.node.v2" // on DPUNode
	// LabelOwnedByDPUDeployment is set by the DPF DPUSet controller on every DPU it
	// creates from a DPUDeployment. NICo lists DPUs by this label when a node's
	// deployment type resolves to a DPUDeployment (the GB200 path), so the
	// simulator must stamp it the same way or NICo never finds the DPU.
	LabelOwnedByDPUDeployment = "svc.dpu.nvidia.com/owned-by-dpudeployment"
)

// Annotations exchanged on the DPF CRs.
const (
	// AnnWaitForExternalNodeEffect is the node-effect hold handshake, carried
	// on the DPUNodeMaintenance CR named "<node-name>-hold" (NOT on the
	// DPUNode). The DPF operator — so here, the simulator — CREATES that CR
	// with this annotation truthy while the DPU is in the Node Effect phase;
	// NICo's release_maintenance_hold (crates/dpf/src/sdk.rs) PATCHES the
	// annotation to the literal string "false" (it never deletes the CR or
	// the key). The simulator parks the DPU in Node Effect until the value is
	// "false" (or the key is absent).
	AnnWaitForExternalNodeEffect = "provisioning.dpu.nvidia.com/wait-for-external-nodeeffect"

	// AnnRebootRequired: the operator/simulator SETS this on the DPUNode when
	// it needs the host rebooted; NICo performs the reboot (via Redfish
	// against the machine-a-tron mock) and CLEARS it (by removing the
	// annotation) ONCE PER NODE after the host powers back on — all DPUs of
	// the node share that one annotation cycle.
	AnnRebootRequired = "provisioning.dpu.nvidia.com/dpunode-external-reboot-required"

	// AnnSimNodeRebootRequestedAt / AnnSimNodeRebootCompletedAt are the
	// simulator's OWN bookkeeping for the reboot handshake, kept on the
	// DPUNode (node-level, because the reboot itself is node-level: NICo
	// clears AnnRebootRequired once per node, and real DPF completes every
	// rebooting DPU on the node from that single cycle).
	//
	// RequestedAt is written in the SAME patch that sets AnnRebootRequired,
	// so a request can never be recorded without its intent marker (no
	// two-object partial-write window). When the simulator later sees
	// RequestedAt present but AnnRebootRequired gone, NICo has completed the
	// cycle: it stamps CompletedAt and drops RequestedAt in one patch. A DPU
	// in the Rebooting phase is satisfied iff CompletedAt is later than its
	// own phase-entry stamp — so one cycle releases every DPU that was
	// already rebooting, while a DPU that enters Rebooting after the cycle
	// finished triggers a fresh one. Not part of the NICo contract —
	// simulator-local.
	AnnSimNodeRebootRequestedAt = "sim.dpu.nvidia.com/node-reboot-requested-at"
	AnnSimNodeRebootCompletedAt = "sim.dpu.nvidia.com/node-reboot-completed-at"

	// AnnSimPhaseEnteredAt records (RFC 3339) when the DPU entered its current
	// phase. Owns() re-enqueues the parent DPUDevice on every DPU status write,
	// so reconciles fire far more often than PhaseDwell; this timestamp is what
	// actually gates dwell phases, not the requeue cadence. Simulator-local.
	AnnSimPhaseEnteredAt = "sim.dpu.nvidia.com/phase-entered-at"
)

// MaintenanceHoldName is the DPUNodeMaintenance CR name for a node's
// node-effect hold, mirrored from crates/dpf/src/sdk.rs release_maintenance_hold
// ("{node_name}-hold").
func MaintenanceHoldName(nodeName string) string { return nodeName + "-hold" }

// CR-name derivation, mirrored from crates/dpf/src/sdk.rs:382-410.
//
//	DPUNode   = "node-{dpfID}"          dpfID = host BMC MAC with ':' -> '-'
//	DPUDevice = "device-{deviceID}"
//	DPU       = "node-{dpfID}-device-{deviceID}"

// DPUNodeName builds the DPUNode CR name NICo creates for a dpf_id.
func DPUNodeName(dpfID string) string { return "node-" + dpfID }

// DPUDeviceName builds the DPUDevice CR name NICo creates for a device_id.
func DPUDeviceName(devID string) string { return "device-" + devID }

// DPUName builds the DPU CR name the operator would create. It concatenates
// the node and device CR names exactly (the device- prefix is intentional).
func DPUName(dpfID, deviceID string) string {
	return DPUNodeName(dpfID) + "-" + DPUDeviceName(deviceID)
}

// NodeIDFromNodeCRName strips the "node-" prefix (sdk.rs:409).
func NodeIDFromNodeCRName(nodeCRName string) string {
	return strings.TrimPrefix(nodeCRName, "node-")
}

// DeviceIDFromDeviceCRName strips the "device-" prefix, yielding the raw
// device_id NICo uses (sdk.rs dpu_cr_name takes the raw id, not the CR name).
func DeviceIDFromDeviceCRName(deviceCRName string) string {
	return strings.TrimPrefix(deviceCRName, "device-")
}

// DPFIDFromBMCMAC converts a BMC MAC (aa:bb:...) into the dpf_id form (aa-bb-...).
func DPFIDFromBMCMAC(mac string) string {
	return strings.ReplaceAll(mac, ":", "-")
}
