// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"encoding/json"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestToStandardExpectedMachineUpdateRequest verifies that nil slice/map fields are not
// forwarded to the standard API request, preventing unintended clearing of existing
// attributes on partial updates.
func TestToStandardExpectedMachineUpdateRequest(t *testing.T) {
	t.Run("partial update with only ChassisSerialNumber set leaves FallbackDPUSerialNumbers and Labels nil", func(t *testing.T) {
		serial := "CHASSIS-001"
		req := ExpectedMachineUpdateRequest{
			ChassisSerialNumber: &serial,
		}

		apiReq := toStandardExpectedMachineUpdateRequest(req)

		assert.True(t, apiReq.ChassisSerialNumber.IsSet())

		// These must stay nil so the server does not clear them
		assert.Nil(t, apiReq.FallbackDPUSerialNumbers,
			"FallbackDPUSerialNumbers must be nil when not set, to avoid clearing existing DPU serial numbers on the server")
		assert.Nil(t, apiReq.Labels,
			"Labels must be nil when not set, to avoid clearing existing labels on the server")
		assert.Nil(t, apiReq.Interfaces,
			"Interfaces must be nil when not set, to avoid clearing existing interfaces on the server")

		body, err := apiReq.ToMap()
		require.NoError(t, err)
		assert.Contains(t, body, "chassisSerialNumber")
		assert.NotContains(t, body, "fallbackDPUSerialNumbers")
		assert.NotContains(t, body, "labels")
		assert.NotContains(t, body, "interfaces")
		assert.NotContains(t, body, "bmcRetainCredentials")
	})

	t.Run("explicit empty interfaces slice is forwarded to clear existing entries", func(t *testing.T) {
		req := ExpectedMachineUpdateRequest{Interfaces: []ExpectedMachineInterface{}}

		apiReq := toStandardExpectedMachineUpdateRequest(req)

		require.NotNil(t, apiReq.Interfaces)
		assert.Empty(t, apiReq.Interfaces)
		body, err := apiReq.ToMap()
		require.NoError(t, err)
		assert.Contains(t, body, "interfaces")
	})

	t.Run("populated interfaces preserve identifiers and optional CX9 data", func(t *testing.T) {
		mac := "02:00:00:00:00:09"
		nicType := "CX9"
		fixedIP := "192.0.2.9"
		mask, gateway, segment, role, allocation := "255.255.255.0", "192.0.2.1", "HostInband", "Host", "Fixed"
		primary := true
		logicalName := "eth_r0_p0"
		req := ExpectedMachineUpdateRequest{Interfaces: []ExpectedMachineInterface{{
			Identifier: &ExpectedMachineInterfaceIdentifier{MacAddress: &mac},
			MacAddress: &mac,
			NicType:    &nicType,
			FixedIP:    &fixedIP,
			FixedMask:  &mask, FixedGateway: &gateway, Primary: &primary,
			NetworkSegmentType: &segment, Role: &role, IPAllocation: &allocation,
		}, {
			Identifier: &ExpectedMachineInterfaceIdentifier{Astra: &ExpectedMachineInterfaceAstraIdentifier{PhysicalName: "C1-1-L1", LogicalName: &logicalName}},
			NicType:    &nicType,
		}}}

		apiReq := toStandardExpectedMachineUpdateRequest(req)

		require.Len(t, apiReq.Interfaces, 2)
		assert.Equal(t, &mac, apiReq.Interfaces[0].Identifier.Get().MacAddress.Get())
		assert.Equal(t, &mac, apiReq.Interfaces[0].MacAddress.Get())
		assert.Equal(t, &nicType, apiReq.Interfaces[0].NicType.Get())
		assert.Equal(t, &fixedIP, apiReq.Interfaces[0].FixedIp.Get())
		astra := apiReq.Interfaces[1].Identifier.Get().Astra.Get()
		require.NotNil(t, astra)
		assert.Equal(t, "C1-1-L1", astra.PhysicalName)
		assert.Equal(t, &logicalName, astra.LogicalName.Get())
		body, err := json.Marshal(apiReq)
		require.NoError(t, err)
		var response standard.ExpectedMachine
		require.NoError(t, json.Unmarshal(body, &response))
		assert.Equal(t, req.Interfaces, expectedMachineFromStandard(response).Interfaces)
	})

	t.Run("partial update with only FallbackDPUSerialNumbers set leaves Labels nil", func(t *testing.T) {
		req := ExpectedMachineUpdateRequest{
			FallbackDPUSerialNumbers: []string{"DPU-001", "DPU-002"},
		}

		apiReq := toStandardExpectedMachineUpdateRequest(req)

		assert.Equal(t, []string{"DPU-001", "DPU-002"}, apiReq.FallbackDPUSerialNumbers)
		assert.Nil(t, apiReq.Labels)

		body, err := apiReq.ToMap()
		require.NoError(t, err)
		assert.Contains(t, body, "fallbackDPUSerialNumbers")
		assert.NotContains(t, body, "labels")
	})

	t.Run("explicit empty FallbackDPUSerialNumbers slice is forwarded to clear existing entries", func(t *testing.T) {
		req := ExpectedMachineUpdateRequest{
			FallbackDPUSerialNumbers: []string{},
		}

		apiReq := toStandardExpectedMachineUpdateRequest(req)

		// Non-nil empty slice must be forwarded so the server clears the list
		require.NotNil(t, apiReq.FallbackDPUSerialNumbers,
			"An explicit empty FallbackDPUSerialNumbers slice must be forwarded to clear existing entries")
		assert.Empty(t, apiReq.FallbackDPUSerialNumbers)

		body, err := apiReq.ToMap()
		require.NoError(t, err)
		assert.Contains(t, body, "fallbackDPUSerialNumbers")
	})

	t.Run("full update sets all provided fields", func(t *testing.T) {
		mac := "AA:BB:CC:DD:EE:FF"
		serial := "CHASSIS-999"
		sku := "sku-123"
		retain := false
		req := ExpectedMachineUpdateRequest{
			BmcMacAddress:            &mac,
			ChassisSerialNumber:      &serial,
			FallbackDPUSerialNumbers: []string{"DPU-A"},
			SkuID:                    &sku,
			Labels:                   map[string]string{"site": "dc1"},
			BmcRetainCredentials:     &retain,
		}

		apiReq := toStandardExpectedMachineUpdateRequest(req)

		assert.True(t, apiReq.BmcMacAddress.IsSet())
		assert.True(t, apiReq.ChassisSerialNumber.IsSet())
		assert.Equal(t, []string{"DPU-A"}, apiReq.FallbackDPUSerialNumbers)
		assert.True(t, apiReq.SkuId.IsSet())
		assert.Equal(t, map[string]string{"site": "dc1"}, apiReq.Labels)

		body, err := apiReq.ToMap()
		require.NoError(t, err)
		assert.Contains(t, body, "bmcMacAddress")
		assert.Contains(t, body, "chassisSerialNumber")
		assert.Contains(t, body, "fallbackDPUSerialNumbers")
		assert.Contains(t, body, "skuId")
		assert.Contains(t, body, "labels")
		assert.Equal(t, &retain, body["bmcRetainCredentials"])
	})
}

func TestToStandardExpectedMachineCreateRequest(t *testing.T) {
	retain := true
	tests := []struct {
		name     string
		request  ExpectedMachineCreateRequest
		wantBody map[string]any
	}{
		{
			name:     "BMC credential retention is forwarded",
			request:  ExpectedMachineCreateRequest{BmcRetainCredentials: &retain},
			wantBody: map[string]any{"bmcRetainCredentials": &retain},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			body, err := toStandardExpectedMachineCreateRequest(tt.request, "f97df110-f4de-492e-8849-4a6af68026b0").ToMap()
			require.NoError(t, err)
			for key, want := range tt.wantBody {
				assert.Equal(t, want, body[key])
			}
		})
	}
}

func TestExpectedMachineFromStandard(t *testing.T) {
	retain := true
	tests := []struct {
		name     string
		response standard.ExpectedMachine
		want     ExpectedMachine
	}{
		{
			name:     "BMC credential retention is read",
			response: standard.ExpectedMachine{BmcRetainCredentials: &retain},
			want:     ExpectedMachine{BmcRetainCredentials: true},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := expectedMachineFromStandard(tt.response)
			assert.Equal(t, tt.want.BmcRetainCredentials, got.BmcRetainCredentials)
		})
	}
}
