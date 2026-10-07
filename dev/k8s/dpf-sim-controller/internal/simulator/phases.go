// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package simulator encodes the DPU phase progression the simulator drives.
//
// The phase names come straight from the upstream constants in
// github.com/nvidia/doca-platform/api/provisioning/v1alpha1 (dpu_types.go) — do
// not invent strings. NICo (crates/machine-controller/src/handler/dpf.rs)
// collapses the intermediate phases into Provisioning(detail) and only takes
// action on Node Effect, Rebooting, Ready, and Error, so walking the authentic
// sequence with a dwell timer is sufficient for the state machine to progress.
//
// The real operator's per-phase logic is upstream in
// internal/provisioning/controllers/dpu/state/*.go (one file per phase). That
// package is internal/ (not importable) but is the definitive reference for
// what each phase actually means — consult it before adding fidelity here.
package simulator

import (
	provisioningv1 "github.com/nvidia/doca-platform/api/provisioning/v1alpha1"
)

// HappyPath is the ordered phase sequence a DPU walks from creation to Ready
// when no error is injected. Node Effect and Rebooting are handled specially by
// the reconciler (they block on external signals) rather than by pure dwell.
//
// Order follows the DPUPhase declaration order in doca-platform v26.4.0
// (api/provisioning/v1alpha1/dpu_types.go); Rebooting — declared out of line
// with the transient phases — is placed after OS Installing, where a DPU
// reboot naturally occurs. There is no DPUUpdateFirmware phase in v26.4.0.
var HappyPath = []provisioningv1.DPUPhase{
	provisioningv1.DPUInitializing,
	provisioningv1.DPUNodeEffect, // gated on the {node}-hold DPUNodeMaintenance (see reconciler)
	provisioningv1.DPUPending,
	provisioningv1.DPUPrepareBFB,
	provisioningv1.DPUConfig,
	provisioningv1.DPUConfigFWParameters,
	provisioningv1.DPUInitializeInterface,
	provisioningv1.DPUOSInstalling,
	provisioningv1.DPURebooting, // sets AnnRebootRequired, waits for NICo to clear
	provisioningv1.DPUClusterConfig,
	provisioningv1.DPUHostNetworkConfiguration,
	provisioningv1.DPUNodeEffectRemoval,
	provisioningv1.DPUReady,
}

// Next returns the phase that follows cur in HappyPath, and ok=false if cur is
// terminal (Ready/Error) or unknown.
func Next(cur provisioningv1.DPUPhase) (provisioningv1.DPUPhase, bool) {
	for i, p := range HappyPath {
		if p == cur && i+1 < len(HappyPath) {
			return HappyPath[i+1], true
		}
	}
	return "", false
}

// IsTerminal reports whether a phase should stop the walker.
func IsTerminal(p provisioningv1.DPUPhase) bool {
	return p == provisioningv1.DPUReady || p == provisioningv1.DPUError
}

// PhaseGate classifies how the reconciler should treat a phase transition.
type PhaseGate int

const (
	// GateDwell: advance after the configured per-phase dwell elapses.
	GateDwell PhaseGate = iota
	// GateHold: stay until the node-effect hold annotation is absent/false.
	GateHold
	// GateReboot: set the reboot-required annotation, stay until NICo clears it.
	GateReboot
)

// Gate returns how a given phase should be gated.
func Gate(p provisioningv1.DPUPhase) PhaseGate {
	switch p {
	case provisioningv1.DPUNodeEffect:
		return GateHold
	case provisioningv1.DPURebooting:
		return GateReboot
	default:
		return GateDwell
	}
}
