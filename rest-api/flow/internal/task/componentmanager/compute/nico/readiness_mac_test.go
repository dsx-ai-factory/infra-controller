// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"context"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/readiness"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/stretchr/testify/require"
)

func TestManager_ensureTargetOperable(t *testing.T) {
	for _, tc := range []struct {
		name     string
		op       types.OperationType
		override bool
	}{
		{"power blocked", types.OperationTypePowerControl, false},
		{"firmware blocked", types.OperationTypeFirmwareControl, false},
		{"explicit override", types.OperationTypePowerControl, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			reader := readiness.NewMemReader()
			reader.SetStatus("mac-1", &types.ComponentOperationStatus{BlockedOperations: []types.OperationType{tc.op}})
			reader.SetRackHosts("mac-1", []string{"mac-1"})
			m := New(nil, readiness.NewDBGate(reader, time.Millisecond, time.Millisecond))
			target := common.Target{Type: devicetypes.ComponentTypeCompute, IdentifierType: common.IdentifierTypeMACAddress, Identifiers: []string{"mac-1"}}
			err := m.ensureTargetOperable(context.Background(), target, tc.op, tc.override)
			if tc.override {
				require.NoError(t, err)
			} else {
				require.ErrorContains(t, err, "timed out")
			}
		})
	}
}
