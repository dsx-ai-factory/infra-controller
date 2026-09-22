// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestBuildTargets(t *testing.T) {
	for _, tc := range []struct {
		name     string
		ids      []string
		wantIDs  []string
		wantType common.IdentifierType
	}{
		{"ingested", []string{"machine-1", "machine-2"}, []string{"machine-1", "machine-2"}, common.IdentifierTypeManagerID},
		{"mixed", []string{"machine-1", ""}, []string{"mac-1", "mac-2"}, common.IdentifierTypeMACAddress},
		{"missing first", []string{"", "machine-2"}, []string{"mac-1", "mac-2"}, common.IdentifierTypeMACAddress},
		{"pre ingestion", []string{"", ""}, []string{"mac-1", "mac-2"}, common.IdentifierTypeMACAddress},
	} {
		t.Run(tc.name, func(t *testing.T) {
			info := &task.ExecutionInfo{Components: []task.WorkflowComponent{
				{Type: devicetypes.ComponentTypeCompute, ComponentID: tc.ids[0], MACAddress: "mac-1"},
				{Type: devicetypes.ComponentTypeCompute, ComponentID: tc.ids[1], MACAddress: "mac-2"},
			}}
			target := buildTargets(info)[devicetypes.ComponentTypeCompute]
			assert.Equal(t, tc.wantIDs, target.Identifiers)
			assert.Equal(t, tc.wantType, target.IdentifierType)
		})
	}
}
