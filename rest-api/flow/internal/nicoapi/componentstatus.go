// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"encoding/json"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// MapComponentOperationStatus translates a raw Core controller_state string into a
// types.ComponentOperationStatus for the given component type. The raw form differs
// per type — that is why this lives in nicoapi (which owns "what raw
// string Core returns") rather than in the dependency-light pkg/types:
//   - Compute: ManagedHostState Display (e.g. "Ready", "Assigned/Provisioning").
//   - Switch / PowerShelf: JSON object with a "state" tag (e.g. {"state":"ready"}).
//
// Unrecognized inputs map to PhaseUnknown so callers fail closed.
func MapComponentOperationStatus(componentType types.ComponentType, rawState string) types.ComponentOperationStatus {
	switch componentType {
	case types.ComponentTypeCompute:
		return mapComputeStatus(rawState)
	case types.ComponentTypeNVSwitch:
		return mapSwitchStatus(rawState)
	case types.ComponentTypePowerShelf:
		return mapPowerShelfStatus(rawState)
	default:
		return types.ComponentOperationStatus{
			Phase:  types.PhaseUnknown,
			Reason: "unsupported component type: " + string(componentType),
		}
	}
}

func mapComputeStatus(raw string) types.ComponentOperationStatus {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return types.ComponentOperationStatus{Phase: types.PhaseUnknown, Reason: "no controller_state from core"}
	}

	head := raw
	i := strings.IndexAny(raw, "/(")
	if i >= 0 {
		head = raw[:i]
	}

	phase, ok := computePhaseByState[head]
	if ok {
		return mappedStatus(phase, raw, types.ComponentTypeCompute)
	}

	// ManagedHostState::Validation Display delegates straight to its
	// inner ValidationState, whose only current variant renders with this
	// prefix. Keep it explicit so a new top-level Core state fails closed
	// until its Flow mapping is deliberately added.
	if isKnownMachineValidationState(raw) {
		return blockAll(types.PhaseInitializing, raw, types.ComponentTypeCompute)
	}

	return types.ComponentOperationStatus{Phase: types.PhaseUnknown, Reason: "unknown compute controller state: " + raw}
}

func isKnownMachineValidationState(raw string) bool {
	const prefix = "MachineValidation { machine_validation: "
	const suffix = " } }"

	inner, ok := strings.CutPrefix(raw, prefix)
	if !ok {
		return false
	}
	inner, ok = strings.CutSuffix(inner, suffix)
	if !ok {
		return false
	}
	variant, fields, ok := strings.Cut(inner, " { ")
	if !ok || fields == "" {
		return false
	}
	_, ok = machineValidationStates[variant]
	return ok
}

// switchStateEnvelope decodes the serde-tagged JSON emitted by core for
// SwitchControllerState / PowerShelfControllerState. Only the "state"
// discriminator is needed for the Phase decision; the full payload is
// kept in Reason for diagnostics.
type switchStateEnvelope struct {
	State string `json:"state"`
}

func mapSwitchStatus(raw string) types.ComponentOperationStatus {
	tag, ok := decodeTaggedState(raw)
	if !ok {
		return types.ComponentOperationStatus{Phase: types.PhaseUnknown, Reason: "undecodable switch state: " + raw}
	}
	phase, ok := switchPhaseByState[tag]
	if ok {
		return mappedStatus(phase, raw, types.ComponentTypeNVSwitch)
	}
	return types.ComponentOperationStatus{Phase: types.PhaseUnknown, Reason: "unknown switch state tag: " + tag}
}

func mapPowerShelfStatus(raw string) types.ComponentOperationStatus {
	tag, ok := decodeTaggedState(raw)
	if !ok {
		return types.ComponentOperationStatus{Phase: types.PhaseUnknown, Reason: "undecodable power shelf state: " + raw}
	}
	phase, ok := powerShelfPhaseByState[tag]
	if ok {
		return mappedStatus(phase, raw, types.ComponentTypePowerShelf)
	}
	return types.ComponentOperationStatus{Phase: types.PhaseUnknown, Reason: "unknown power shelf state tag: " + tag}
}

// These maps are the compatibility boundary between each Core controller state
// machine and Flow's shared operability phases. Keep each table aligned with
// its corresponding Core enum when states are added or renamed.
var computePhaseByState = map[string]types.Phase{
	"Ready":                 types.PhaseReady,
	"StartAssignmentCycle":  types.PhaseReady,
	"Created":               types.PhaseInitializing,
	"DPUDiscovering":        types.PhaseInitializing,
	"DPUInitializing":       types.PhaseInitializing,
	"HostInitializing":      types.PhaseInitializing,
	"Measuring":             types.PhaseInitializing,
	"PreAssignedMeasuring":  types.PhaseInitializing,
	"PostAssignedMeasuring": types.PhaseInitializing,
	"BomValidating":         types.PhaseInitializing,
	"Assigned":              types.PhaseInUse,
	"WaitingForCleanup":     types.PhaseInUse,
	"Reprovisioning":        types.PhaseInUse,
	"HostReprovisioning":    types.PhaseInUse,
	"ConfigureAstra":        types.PhaseInUse,
	"BootConfiguring":       types.PhaseInUse,
	"Maintenance":           types.PhaseInUse,
	"RotatingBmc":           types.PhaseInUse,
	"RotatingHostUefi":      types.PhaseInUse,
	"RotatingDpuUefi":       types.PhaseInUse,
	"RotatingNicLockdown":   types.PhaseInUse,
	"Failed":                types.PhaseError,
	"Decommissioning":       types.PhaseDeleting,
	"ForceDeletion":         types.PhaseDeleting,
}

var machineValidationStates = map[string]struct{}{
	"RebootHost":               {},
	"MachineValidating":        {},
	"PrepareBootRepair":        {},
	"UnlockForBootRepair":      {},
	"CheckBootConfigForRepair": {},
	"ConfigureBootBios":        {},
	"WaitingForBootBiosJob":    {},
	"PollingBootBiosSetup":     {},
	"RepairBootConfig":         {},
	"LockAfterBootRepair":      {},
}

var switchPhaseByState = map[string]types.Phase{
	"created":         types.PhaseInitializing,
	"initializing":    types.PhaseInitializing,
	"configuring":     types.PhaseInitializing,
	"fetchinfo":       types.PhaseInitializing,
	"validating":      types.PhaseInitializing,
	"bomvalidating":   types.PhaseInitializing,
	"ready":           types.PhaseReady,
	"rotatingbmc":     types.PhaseInUse,
	"maintenance":     types.PhaseInUse,
	"reprovisioning":  types.PhaseInUse,
	"error":           types.PhaseError,
	"decommissioning": types.PhaseDeleting,
	"deleting":        types.PhaseDeleting,
}

var powerShelfPhaseByState = map[string]types.Phase{
	"initializing":    types.PhaseInitializing,
	"fetchingdata":    types.PhaseInitializing,
	"configuring":     types.PhaseInitializing,
	"ready":           types.PhaseReady,
	"rotatingbmc":     types.PhaseInUse,
	"maintenance":     types.PhaseInUse,
	"reprovisioning":  types.PhaseInUse,
	"error":           types.PhaseError,
	"decommissioning": types.PhaseDeleting,
	"deleting":        types.PhaseDeleting,
}

func decodeTaggedState(raw string) (string, bool) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return "", false
	}
	var env switchStateEnvelope
	if err := json.Unmarshal([]byte(raw), &env); err != nil || env.State == "" {
		return "", false
	}
	return env.State, true
}

// blockedOpsByType lists the operations Flow currently knows how to gate
// per component type. When Phase != Ready, all of these are blocked;
// Ready blocks none. Per-operation refinement (e.g. allowing power while
// a compute is in Assigned/Provisioning) is deferred.
var blockedOpsByType = map[types.ComponentType][]types.OperationType{
	types.ComponentTypeCompute:    {types.OperationTypePowerControl, types.OperationTypeFirmwareControl},
	types.ComponentTypeNVSwitch:   {types.OperationTypePowerControl, types.OperationTypeFirmwareControl},
	types.ComponentTypePowerShelf: {types.OperationTypePowerControl, types.OperationTypeFirmwareControl},
}

func blockAll(phase types.Phase, reason string, ct types.ComponentType) types.ComponentOperationStatus {
	return types.ComponentOperationStatus{
		Phase:             phase,
		Reason:            reason,
		BlockedOperations: append([]types.OperationType(nil), blockedOpsByType[ct]...),
	}
}

func blockNoneIfReady(phase types.Phase, reason string, _ types.ComponentType) types.ComponentOperationStatus {
	return types.ComponentOperationStatus{Phase: phase, Reason: reason}
}

func mappedStatus(phase types.Phase, raw string, componentType types.ComponentType) types.ComponentOperationStatus {
	if phase == types.PhaseReady {
		return blockNoneIfReady(phase, "", componentType)
	}
	return blockAll(phase, raw, componentType)
}
