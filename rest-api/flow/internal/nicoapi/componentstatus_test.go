// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"testing"

	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

func TestMapComponentOperationStatus_Compute(t *testing.T) {
	cases := []struct {
		name      string
		raw       string
		wantPhase types.Phase
		wantOps   []types.OperationType
	}{
		// Steady state — no operations blocked.
		{"ready", "Ready", types.PhaseReady, nil},
		{"start_assignment_cycle", "StartAssignmentCycle", types.PhaseReady, nil},

		// Initializing buckets — top-level Display heads.
		{"created", "Created", types.PhaseInitializing, allComputeOps()},
		{"dpu_discovering", "DPUDiscovering/Unknown", types.PhaseInitializing, allComputeOps()},
		{"dpu_initializing", "DPUInitializing/Init", types.PhaseInitializing, allComputeOps()},
		{"host_initializing", "HostInitializing/Init", types.PhaseInitializing, allComputeOps()},
		{"measuring", "Measuring/Boot", types.PhaseInitializing, allComputeOps()},
		{"pre_assigned_measuring", "PreAssignedMeasuring/Idle", types.PhaseInitializing, allComputeOps()},
		{"post_assigned_measuring", "PostAssignedMeasuring/MeasuredBoot/Idle", types.PhaseInitializing, allComputeOps()},
		{"bom_validating", "BomValidating/Some", types.PhaseInitializing, allComputeOps()},

		// InUse buckets — tenant owns the host, or core is mid-reprovision.
		{"assigned_ready", "Assigned/Ready", types.PhaseInUse, allComputeOps()},
		{"assigned_provisioning", "Assigned/Provisioning", types.PhaseInUse, allComputeOps()},
		{"assigned_reprovision", "Assigned/Reprovision/Init", types.PhaseInUse, allComputeOps()},
		{"waiting_for_cleanup", "WaitingForCleanup/Init", types.PhaseInUse, allComputeOps()},
		{"dpu_reprovision", "Reprovisioning/Init", types.PhaseInUse, allComputeOps()},
		{"host_reprovision", "HostReprovisioning/Init", types.PhaseInUse, allComputeOps()},
		{"configure_astra", "ConfigureAstra/Init", types.PhaseInUse, allComputeOps()},
		{"boot_configuring", "BootConfiguring/Init", types.PhaseInUse, allComputeOps()},
		{"maintenance", "Maintenance(FirmwareUpdate)", types.PhaseInUse, allComputeOps()},
		{"rotating_bmc", "RotatingBmc", types.PhaseInUse, allComputeOps()},
		{"rotating_host_uefi", "RotatingHostUefi/Init", types.PhaseInUse, allComputeOps()},
		{"rotating_dpu_uefi", "RotatingDpuUefi/machine-1", types.PhaseInUse, allComputeOps()},
		{"rotating_nic_lockdown", "RotatingNicLockdown", types.PhaseInUse, allComputeOps()},

		// Terminal.
		{"failed", "Failed/SomeCause", types.PhaseError, allComputeOps()},
		{"decommissioning", "Decommissioning/SuppressingSiteExplorer", types.PhaseDeleting, allComputeOps()},
		{"force_deletion", "ForceDeletion", types.PhaseDeleting, allComputeOps()},

		// Defaults.
		{"empty", "", types.PhaseUnknown, nil},
		// Validation Display delegates to ValidationState's Debug representation.
		{
			"machine_validation",
			"MachineValidation { machine_validation: RebootHost { validation_id: MachineValidationId(00000000-0000-0000-0000-000000000001) } }",
			types.PhaseInitializing,
			allComputeOps(),
		},
		{"machine_validation_incomplete", "MachineValidation {", types.PhaseUnknown, nil},
		{
			"machine_validation_unknown_variant",
			"MachineValidation { machine_validation: WarpDrive { value: 1 } }",
			types.PhaseUnknown,
			nil,
		},
		{"unknown", "WarpDrive", types.PhaseUnknown, nil},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := MapComponentOperationStatus(types.ComponentTypeCompute, tc.raw)
			assert.Equal(t, tc.wantPhase, got.Phase, "phase")
			assert.Equal(t, tc.wantOps, got.BlockedOperations, "blocked ops")
		})
	}
}

func TestMapComponentOperationStatus_Switch(t *testing.T) {
	cases := []struct {
		name      string
		raw       string
		wantPhase types.Phase
		wantOps   []types.OperationType
	}{
		{"ready", `{"state":"ready"}`, types.PhaseReady, nil},
		{"created", `{"state":"created"}`, types.PhaseInitializing, allNVSwitchOps()},
		{"initializing", `{"state":"initializing"}`, types.PhaseInitializing, allNVSwitchOps()},
		{"configuring", `{"state":"configuring"}`, types.PhaseInitializing, allNVSwitchOps()},
		{"fetch_info", `{"state":"fetchinfo"}`, types.PhaseInitializing, allNVSwitchOps()},
		{"validating", `{"state":"validating"}`, types.PhaseInitializing, allNVSwitchOps()},
		{"bomvalidating", `{"state":"bomvalidating"}`, types.PhaseInitializing, allNVSwitchOps()},
		{"rotating_bmc", `{"state":"rotatingbmc","retry_count":0}`, types.PhaseInUse, allNVSwitchOps()},
		{"maintenance", `{"state":"maintenance","operation":"reconfigurecertificate"}`, types.PhaseInUse, allNVSwitchOps()},
		{
			"reprovisioning_with_substate",
			`{"state":"reprovisioning","reprovisioning_state":"WaitingForRackFirmwareUpgrade"}`,
			types.PhaseInUse,
			allNVSwitchOps(),
		},
		{"error", `{"state":"error"}`, types.PhaseError, allNVSwitchOps()},
		{"decommissioning", `{"state":"decommissioning"}`, types.PhaseDeleting, allNVSwitchOps()},
		{"deleting", `{"state":"deleting"}`, types.PhaseDeleting, allNVSwitchOps()},

		// Invalid / unknown — fail closed.
		{"empty", "", types.PhaseUnknown, nil},
		{"garbage", "not-json", types.PhaseUnknown, nil},
		{"unknown_tag", `{"state":"warpdrive"}`, types.PhaseUnknown, nil},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := MapComponentOperationStatus(types.ComponentTypeNVSwitch, tc.raw)
			assert.Equal(t, tc.wantPhase, got.Phase, "phase")
			assert.Equal(t, tc.wantOps, got.BlockedOperations, "blocked ops")
		})
	}
}

func TestMapComponentOperationStatus_PowerShelf(t *testing.T) {
	cases := []struct {
		name      string
		raw       string
		wantPhase types.Phase
		wantOps   []types.OperationType
	}{
		{"ready", `{"state":"ready"}`, types.PhaseReady, nil},
		{"initializing", `{"state":"initializing"}`, types.PhaseInitializing, allPowerShelfOps()},
		{"fetching_data", `{"state":"fetchingdata"}`, types.PhaseInitializing, allPowerShelfOps()},
		{"configuring", `{"state":"configuring"}`, types.PhaseInitializing, allPowerShelfOps()},
		{"rotating_bmc", `{"state":"rotatingbmc","retry_count":0}`, types.PhaseInUse, allPowerShelfOps()},
		{
			"maintenance_with_op",
			`{"state":"maintenance","maintenance":{"operation":"poweron"}}`,
			types.PhaseInUse,
			allPowerShelfOps(),
		},
		{
			"reprovisioning",
			`{"state":"reprovisioning","reprovisioning_state":"WaitingForRackFirmwareUpgrade"}`,
			types.PhaseInUse,
			allPowerShelfOps(),
		},
		{"error", `{"state":"error"}`, types.PhaseError, allPowerShelfOps()},
		{"decommissioning", `{"state":"decommissioning"}`, types.PhaseDeleting, allPowerShelfOps()},
		{"deleting", `{"state":"deleting"}`, types.PhaseDeleting, allPowerShelfOps()},

		{"empty", "", types.PhaseUnknown, nil},
		{"garbage", "{", types.PhaseUnknown, nil},
		{"unknown_tag", `{"state":"breakdancing"}`, types.PhaseUnknown, nil},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := MapComponentOperationStatus(types.ComponentTypePowerShelf, tc.raw)
			assert.Equal(t, tc.wantPhase, got.Phase, "phase")
			assert.Equal(t, tc.wantOps, got.BlockedOperations, "blocked ops")
		})
	}
}

func TestMapComponentOperationStatus_UnsupportedType(t *testing.T) {
	got := MapComponentOperationStatus(types.ComponentTypeTORSwitch, `{"state":"ready"}`)
	assert.Equal(t, types.PhaseUnknown, got.Phase)
	assert.Empty(t, got.BlockedOperations)
}

func allComputeOps() []types.OperationType {
	return []types.OperationType{types.OperationTypePowerControl, types.OperationTypeFirmwareControl}
}

func allNVSwitchOps() []types.OperationType {
	return []types.OperationType{types.OperationTypePowerControl, types.OperationTypeFirmwareControl}
}

func allPowerShelfOps() []types.OperationType {
	return []types.OperationType{types.OperationTypePowerControl, types.OperationTypeFirmwareControl}
}
