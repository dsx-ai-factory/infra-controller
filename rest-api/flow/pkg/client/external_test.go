// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package client_test verifies that the client package can be imported and used
// by external Go modules. This test uses the _test package suffix to simulate
// an external consumer - it can only access exported (public) symbols.
//
// If this test fails to compile, it likely means the client package has
// introduced a dependency on an internal package, breaking external usability.
package client_test

import (
	"testing"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// TestExternalUsability verifies that all public types and functions are
// accessible from an external package. This is a compile-time check - if
// the client package imports from internal/, this test won't compile when
// used from an external module.
func TestExternalUsability(t *testing.T) {
	// Verify Config is accessible and can be created
	cfg := client.Config{
		Host: "localhost",
		Port: 50051,
	}

	if err := cfg.Validate(); err != nil {
		// Expected to pass validation
		t.Errorf("Config.Validate() failed: %v", err)
	}

	if target := cfg.Target(); target != "localhost:50051" {
		t.Errorf("Config.Target() = %q, want %q", target, "localhost:50051")
	}

	// Verify types package types are usable with client
	_ = types.Rack{
		Info: types.DeviceInfo{
			ID:           uuid.New(),
			Name:         "test-rack",
			Manufacturer: "NVIDIA",
			Model:        "GB200",
			SerialNumber: "SN123",
		},
		Location: types.Location{
			Region:     "US",
			Datacenter: "DC1",
		},
	}

	_ = types.Component{
		Type: types.ComponentTypeCompute,
		Info: types.DeviceInfo{
			ID:           uuid.New(),
			Name:         "compute-0",
			Manufacturer: "NVIDIA",
			SerialNumber: "COMP123",
		},
		Position: types.InRackPosition{
			SlotID:    1,
			TrayIndex: 0,
			HostID:    0,
		},
	}

	// Verify result types are accessible
	_ = client.UpgradeFirmwareResult{}
	_ = client.PowerControlResult{}
	_ = client.GetExpectedComponentsResult{}
	_ = client.ValidateComponentsResult{}
	_ = client.ListTasksResult{}
	_ = (*client.Client).UpgradeFirmwareByNVLDomainIDs
	_ = (*client.Client).UpgradeFirmwareByNVLDomainNames
	_ = (*client.Client).PowerControlByNVLDomainIDs
	_ = (*client.Client).PowerControlByNVLDomainNames

	// Verify enum types from types package
	_ = types.ComponentTypeCompute
	_ = types.ComponentTypeNVSwitch
	_ = types.ComponentTypePowerShelf
	_ = types.PowerControlOpOn
	_ = types.PowerControlOpOff
	_ = types.TaskStatusPending
	_ = types.DiffTypeMismatch
}

// TestClientCreationFailsWithoutServer verifies client creation behavior.
// We don't actually connect - just verify the API is accessible.
func TestClientCreationFailsWithoutServer(t *testing.T) {
	cfg := client.Config{
		Host: "localhost",
		Port: 50051,
	}

	// Client creation should work (it's lazy connection)
	c, err := client.New(cfg)
	if err != nil {
		t.Fatalf("client.New() failed: %v", err)
	}
	defer c.Close()
}

// TestTypesInteroperability verifies types package works with client package.
func TestTypesInteroperability(t *testing.T) {
	// These type assertions verify the types are compatible
	var _ *types.Rack
	var _ *types.Component
	var _ *types.ComponentDiff
	var _ *types.Task
	var _ *types.Pagination
	var _ *types.StringQueryInfo
	var _ *types.Identifier
	var _ *types.NVLDomain

	// Verify all component types are accessible
	componentTypes := []types.ComponentType{
		types.ComponentTypeUnknown,
		types.ComponentTypeCompute,
		types.ComponentTypeNVSwitch,
		types.ComponentTypePowerShelf,
		types.ComponentTypeTORSwitch,
		types.ComponentTypeUMS,
		types.ComponentTypeCDU,
	}
	if len(componentTypes) == 0 {
		t.Error("no component types")
	}

	// Verify all power control ops are accessible
	powerOps := []types.PowerControlOp{
		types.PowerControlOpOn,
		types.PowerControlOpForceOn,
		types.PowerControlOpOff,
		types.PowerControlOpForceOff,
		types.PowerControlOpRestart,
		types.PowerControlOpForceRestart,
		types.PowerControlOpWarmReset,
		types.PowerControlOpColdReset,
	}
	if len(powerOps) == 0 {
		t.Error("no power ops")
	}

	// Verify task statuses are accessible
	taskStatuses := []types.TaskStatus{
		types.TaskStatusUnknown,
		types.TaskStatusPending,
		types.TaskStatusRunning,
		types.TaskStatusCompleted,
		types.TaskStatusFailed,
	}
	if len(taskStatuses) == 0 {
		t.Error("no task statuses")
	}

	// Verify diff types are accessible
	diffTypes := []types.DiffType{
		types.DiffTypeUnknown,
		types.DiffTypeMissing,
		types.DiffTypeUnexpected,
		types.DiffTypeMismatch,
	}
	if len(diffTypes) == 0 {
		t.Error("no diff types")
	}

	// Verify BMC types are accessible
	bmcTypes := []types.BMCType{
		types.BMCTypeUnknown,
		types.BMCTypeHost,
		types.BMCTypeDPU,
	}
	if len(bmcTypes) == 0 {
		t.Error("no bmc types")
	}

	// Verify task executor types are accessible
	executorTypes := []types.TaskExecutorType{
		types.TaskExecutorTypeUnknown,
		types.TaskExecutorTypeTemporal,
	}
	if len(executorTypes) == 0 {
		t.Error("no executor types")
	}
}

// TestComponentHasPowerState verifies Component includes power_state field.
func TestComponentHasPowerState(t *testing.T) {
	c := types.Component{
		PowerState: "on",
	}
	if c.PowerState == "" {
		t.Error("PowerState should not be empty")
	}
}
