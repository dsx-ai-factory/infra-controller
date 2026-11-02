// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"context"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/require"
	"testing"
)

type inventoryResponseClient struct {
	nicoapi.Client
	response *corev1.GetComponentInventoryResponse
}

func (c inventoryResponseClient) GetComponentInventory(context.Context, *corev1.GetComponentInventoryRequest) (*corev1.GetComponentInventoryResponse, error) {
	return c.response, nil
}
func TestManager_GetPowerStatus(t *testing.T) {
	for _, useMAC := range []bool{false, true} {
		name := "ID"
		if useMAC {
			name = "MAC"
		}
		t.Run(name, func(t *testing.T) {
			id, mac := "component-1", "aa:bb:cc:dd:ee:01"
			failedID, failedMAC := "component-2", "aa:bb:cc:dd:ee:02"
			target := common.Target{Type: devicetypes.ComponentTypeNVSwitch, Identifiers: []string{id, failedID, "component-3"}}
			key := id
			if useMAC {
				target.IdentifierType = common.IdentifierTypeMACAddress
				target.Identifiers = []string{mac, failedMAC, "aa:bb:cc:dd:ee:03"}
				key = mac
			}
			client := inventoryResponseClient{response: &corev1.GetComponentInventoryResponse{Entries: []*corev1.ComponentInventoryEntry{
				{Result: &corev1.ComponentResult{ComponentId: &id, MacAddress: &mac}},
				{Result: &corev1.ComponentResult{ComponentId: &failedID, MacAddress: &failedMAC, Status: corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_NOT_FOUND}},
				{},
			}}}
			got, err := New(client, nil).GetPowerStatus(context.Background(), target)
			require.NoError(t, err)
			// A successful response with no known state is retained; failed and absent
			// responses must not be synthesized into entries.
			require.Equal(t, map[string]operations.PowerStatus{key: operations.PowerStatusUnknown}, got)
		})
	}
}
