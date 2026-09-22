// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

type powerProvisionerStub struct {
	calls  []string
	failAt map[string]error
}

func (s *powerProvisionerStub) call(value string) error {
	s.calls = append(s.calls, value)
	return s.failAt[value]
}

func (s *powerProvisionerStub) ListPowerProfiles(context.Context) ([]string, error) {
	return nil, s.call("list")
}

func (s *powerProvisionerStub) ValidateAllocation(_ context.Context, machineIDs []string, powerProfile string) error {
	return s.call("validate:" + powerProfile + ":" + strings.Join(machineIDs, ","))
}

func (s *powerProvisionerStub) CreateResourceGroup(_ context.Context, resourceGroup string, _ int64) error {
	return s.call("create:" + resourceGroup)
}

func (s *powerProvisionerStub) DeleteResourceGroup(_ context.Context, resourceGroup string) error {
	return s.call("delete:" + resourceGroup)
}

func (s *powerProvisionerStub) AddMachine(_ context.Context, resourceGroup, machineID, powerProfile string) error {
	return s.call("add:" + resourceGroup + ":" + machineID + ":" + powerProfile)
}

func (s *powerProvisionerStub) AddMachines(_ context.Context, resourceGroup string, machineIDs []string, powerProfile string) error {
	return s.call("add:" + resourceGroup + ":" + strings.Join(machineIDs, ",") + ":" + powerProfile)
}

func (s *powerProvisionerStub) UpdateMachineProfile(_ context.Context, resourceGroup, machineID, powerProfile string) error {
	return s.call("update:" + resourceGroup + ":" + machineID + ":" + powerProfile)
}

func (s *powerProvisionerStub) RemoveMachine(_ context.Context, resourceGroup, machineID string) error {
	return s.call("remove:" + resourceGroup + ":" + machineID)
}

func (s *powerProvisionerStub) RemoveMachines(_ context.Context, resourceGroup string, machineIDs []string) error {
	return s.call("remove:" + resourceGroup + ":" + strings.Join(machineIDs, ","))
}

func (s *powerProvisionerStub) ActivateResourceGroup(_ context.Context, resourceGroup string) error {
	return s.call("activate:" + resourceGroup)
}

func TestProvisionMachinePower(t *testing.T) {
	tests := []struct {
		name          string
		stub          *powerProvisionerStub
		assignment    MachinePowerAssignment
		expectedError string
		rollback      bool
		expectedCalls []string
	}{
		{
			name:       "authorizes before mutation and compensates",
			stub:       &powerProvisionerStub{},
			assignment: MachinePowerAssignment{MachineID: "machine-a", PowerProfile: "performance"},
			rollback:   true,
			expectedCalls: []string{
				"validate:performance:machine-a", "add:group-a:machine-a:performance",
				"activate:group-a", "remove:group-a:machine-a",
			},
		},
		{
			name:          "cleans up failed activation",
			stub:          &powerProvisionerStub{failAt: map[string]error{"activate:group-a": errors.New("denied")}},
			assignment:    MachinePowerAssignment{MachineID: "machine-a"},
			expectedError: "activate DPS resource group",
			expectedCalls: []string{"add:group-a:machine-a:", "activate:group-a", "remove:group-a:machine-a"},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			rollback, err := ProvisionMachinePower(context.Background(), test.stub, "group-a", test.assignment)
			if test.expectedError != "" {
				require.ErrorContains(t, err, test.expectedError)
				assert.Nil(t, rollback)
			} else {
				require.NoError(t, err)
				require.NotNil(t, rollback)
				if test.rollback {
					require.NoError(t, rollback())
				}
			}
			assert.Equal(t, test.expectedCalls, test.stub.calls)
		})
	}
}

func TestProvisionMachineBatchPower(t *testing.T) {
	assignments := []MachinePowerAssignment{
		{MachineID: "machine-a", PowerProfile: "performance"},
		{MachineID: "machine-b", PowerProfile: "performance"},
	}
	tests := []struct {
		name          string
		stub          *powerProvisionerStub
		expectedError string
		rollback      bool
		expectedCalls []string
	}{
		{
			name:     "authorizes before mutation and compensates",
			stub:     &powerProvisionerStub{},
			rollback: true,
			expectedCalls: []string{
				"validate:performance:machine-a,machine-b", "add:group-a:machine-a,machine-b:performance",
				"activate:group-a", "remove:group-a:machine-a,machine-b",
			},
		},
		{
			name:          "does not mutate after rejected preflight",
			stub:          &powerProvisionerStub{failAt: map[string]error{"validate:performance:machine-a,machine-b": errors.New("denied")}},
			expectedError: "validate DPS batch allocation",
			expectedCalls: []string{"validate:performance:machine-a,machine-b"},
		},
		{
			name:          "cleans up a failed batch add",
			stub:          &powerProvisionerStub{failAt: map[string]error{"add:group-a:machine-a,machine-b:performance": errors.New("unavailable")}},
			expectedError: "add machines to DPS resource group",
			expectedCalls: []string{
				"validate:performance:machine-a,machine-b", "add:group-a:machine-a,machine-b:performance",
				"remove:group-a:machine-a,machine-b",
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			rollback, err := ProvisionMachineBatchPower(context.Background(), test.stub, "group-a", assignments)
			if test.expectedError != "" {
				require.ErrorContains(t, err, test.expectedError)
				assert.Nil(t, rollback)
			} else {
				require.NoError(t, err)
				require.NotNil(t, rollback)
				if test.rollback {
					require.NoError(t, rollback())
				}
			}
			assert.Equal(t, test.expectedCalls, test.stub.calls)
		})
	}
}

func TestUpdateMachinePower(t *testing.T) {
	tests := []struct {
		name          string
		powerProfile  string
		previous      string
		expectedCalls []string
	}{
		{
			name:          "clear remains a DPS-authorized mutation",
			previous:      "performance",
			expectedCalls: []string{"update:group-a:machine-a:", "update:group-a:machine-a:performance"},
		},
		{
			name:          "set validates before updating",
			powerProfile:  "performance",
			expectedCalls: []string{"validate:performance:machine-a", "update:group-a:machine-a:performance", "update:group-a:machine-a:"},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			stub := &powerProvisionerStub{}
			rollback, err := UpdateMachinePower(context.Background(), stub, "group-a", MachinePowerAssignment{MachineID: "machine-a", PowerProfile: test.powerProfile}, test.previous)
			require.NoError(t, err)
			require.NoError(t, rollback())
			assert.Equal(t, test.expectedCalls, stub.calls)
		})
	}
}

func TestPreparePowerResourceGroupChange(t *testing.T) {
	t.Run("migrates and completes", func(t *testing.T) {
		stub := &powerProvisionerStub{}
		change, err := PreparePowerResourceGroupChange(context.Background(), stub, 42, "group-a", "group-b", []MachinePowerAssignment{
			{MachineID: "machine-a", PowerProfile: "performance"},
			{MachineID: "machine-b"},
		})
		require.NoError(t, err)
		require.NoError(t, change.Complete())
		assert.Equal(t, []string{
			"validate:performance:machine-a",
			"create:group-b",
			"remove:group-a:machine-a",
			"add:group-b:machine-a:performance",
			"remove:group-a:machine-b",
			"add:group-b:machine-b:",
			"activate:group-b",
			"delete:group-a",
		}, stub.calls)
	})

	t.Run("rolls back prepared migration", func(t *testing.T) {
		stub := &powerProvisionerStub{}
		change, err := PreparePowerResourceGroupChange(context.Background(), stub, 42, "group-a", "group-b", []MachinePowerAssignment{{MachineID: "machine-a", PowerProfile: "performance"}})
		require.NoError(t, err)
		require.NoError(t, change.Rollback())
		assert.Equal(t, []string{
			"validate:performance:machine-a",
			"create:group-b",
			"remove:group-a:machine-a",
			"add:group-b:machine-a:performance",
			"activate:group-b",
			"remove:group-b:machine-a",
			"add:group-a:machine-a:performance",
			"activate:group-a",
			"delete:group-b",
		}, stub.calls)
	})

	t.Run("validates profiles deterministically", func(t *testing.T) {
		stub := &powerProvisionerStub{failAt: map[string]error{
			"validate:balanced:machine-b": errors.New("denied"),
		}}
		change, err := PreparePowerResourceGroupChange(context.Background(), stub, 42, "group-a", "group-b", []MachinePowerAssignment{
			{MachineID: "machine-a", PowerProfile: "performance"},
			{MachineID: "machine-b", PowerProfile: "balanced"},
		})
		require.ErrorContains(t, err, "validate DPS resource-group migration")
		assert.Nil(t, change)
		assert.Equal(t, []string{"validate:balanced:machine-b"}, stub.calls)
	})

	t.Run("reactivates old group after restoring first failed move", func(t *testing.T) {
		stub := &powerProvisionerStub{failAt: map[string]error{"add:group-b:machine-a:performance": errors.New("denied")}}
		change, err := PreparePowerResourceGroupChange(context.Background(), stub, 42, "group-a", "group-b", []MachinePowerAssignment{{MachineID: "machine-a", PowerProfile: "performance"}})
		require.ErrorContains(t, err, "add machine to replacement DPS resource group")
		assert.Nil(t, change)
		assert.Equal(t, []string{
			"validate:performance:machine-a",
			"create:group-b",
			"remove:group-a:machine-a",
			"add:group-b:machine-a:performance",
			"add:group-a:machine-a:performance",
			"activate:group-a",
			"delete:group-b",
		}, stub.calls)
	})

	t.Run("clears before commit and can roll back", func(t *testing.T) {
		stub := &powerProvisionerStub{}
		change, err := PreparePowerResourceGroupChange(context.Background(), stub, 42, "group-a", "", []MachinePowerAssignment{
			{MachineID: "machine-a", PowerProfile: "performance"},
		})
		require.NoError(t, err)
		require.NoError(t, change.Rollback())
		assert.Equal(t, []string{
			"remove:group-a:machine-a",
			"add:group-a:machine-a:performance",
			"activate:group-a",
		}, stub.calls)

		stub.calls = nil
		change, err = PreparePowerResourceGroupChange(context.Background(), stub, 42, "group-a", "", []MachinePowerAssignment{
			{MachineID: "machine-a", PowerProfile: "performance"},
		})
		require.NoError(t, err)
		require.NoError(t, change.Complete())
		assert.Equal(t, []string{
			"remove:group-a:machine-a",
			"delete:group-a",
		}, stub.calls)
	})
}
