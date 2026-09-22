// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicolegacy

import (
	"context"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// Any call through the embedded nil client panics, proving rejection occurs
// before a legacy API is invoked, including temporary health overrides.
type noCallsClient struct {
	nicoapi.Client
}

type powerStatusClient struct {
	nicoapi.Client
	queriedIDs []string
}

func (c *powerStatusClient) GetPowerStates(_ context.Context, ids []string) ([]nicoapi.MachinePowerState, error) {
	c.queriedIDs = ids
	return []nicoapi.MachinePowerState{{MachineID: "machine-1", PowerState: nicoapi.PowerStateOn}}, nil
}

func TestManager_GetPowerStatus(t *testing.T) {
	for _, tc := range []struct {
		name       string
		kind       common.IdentifierType
		identifier string
		wantError  bool
	}{
		{"old payload retains machine ID path", common.IdentifierTypeLegacy, "machine-1", false},
		{"explicit machine ID", common.IdentifierTypeManagerID, "machine-1", false},
		{"MAC rejected before Core call", common.IdentifierTypeMACAddress, "aa:bb:cc:dd:ee:01", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client := &powerStatusClient{}
			target := common.Target{Type: devicetypes.ComponentTypeCompute, IdentifierType: tc.kind, Identifiers: []string{tc.identifier}}
			got, err := New(client, 0, nil).GetPowerStatus(context.Background(), target)
			if tc.wantError {
				require.ErrorContains(t, err, "MAC-address targets are not supported by the nicolegacy compute manager")
				require.Nil(t, got)
				require.Nil(t, client.queriedIDs)
				return
			}
			require.NoError(t, err)
			require.Equal(t, target.Identifiers, client.queriedIDs)
			require.Equal(t, map[string]operations.PowerStatus{"machine-1": operations.PowerStatusOn}, got)
		})
	}
}

func TestManager_PowerControl(t *testing.T) {
	for name, ids := range map[string][]string{
		"MAC only":                      {""},
		"mixed ingested and uningested": {"machine-1", ""},
	} {
		t.Run(name, func(t *testing.T) {
			target := common.Target{Type: devicetypes.ComponentTypeCompute, IdentifierType: common.IdentifierTypeMACAddress,
				Identifiers: []string{"aa:bb:cc:dd:ee:01", "aa:bb:cc:dd:ee:02"}[:len(ids)]}
			require.NoError(t, target.Validate())
			m := New(noCallsClient{}, 0, nil)
			err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
				Operation:              operations.PowerOperationPowerOn,
				OverrideReadinessCheck: true,
			})
			require.ErrorContains(t, err, "MAC-address targets are not supported by the nicolegacy compute manager")
		})
	}
}

func TestManager_FirmwareControl(t *testing.T) {
	for name, ids := range map[string][]string{
		"MAC only":                      {""},
		"mixed ingested and uningested": {"machine-1", ""},
	} {
		t.Run(name, func(t *testing.T) {
			target := common.Target{Type: devicetypes.ComponentTypeCompute, IdentifierType: common.IdentifierTypeMACAddress,
				Identifiers: []string{"aa:bb:cc:dd:ee:01", "aa:bb:cc:dd:ee:02"}[:len(ids)]}
			require.NoError(t, target.Validate())
			m := New(noCallsClient{}, 0, nil)
			err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
				Operation:              operations.FirmwareOperationUpgrade,
				OverrideReadinessCheck: true,
			})
			require.ErrorContains(t, err, "MAC-address targets are not supported by the nicolegacy compute manager")
		})
	}
}
