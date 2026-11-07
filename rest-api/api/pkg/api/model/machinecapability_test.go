// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/stretchr/testify/assert"
)

func TestMachineCapability_NewAPIMachineCapability(t *testing.T) {
	dbmc := &cdbm.MachineCapability{
		Type:      cdbm.MachineCapabilityTypeCPU,
		Name:      "AMD Opteron Series x10",
		Frequency: cutil.GetPtr("3.0GHz"),
		Capacity:  cutil.GetPtr("3.0GHz"),
		Vendor:    cutil.GetPtr("AMD"),
		Count:     cutil.GetPtr(2),
	}

	apimc := NewAPIMachineCapability(dbmc)
	assert.Equal(t, dbmc.Type, apimc.Type)
	assert.Equal(t, dbmc.Name, apimc.Name)
	assert.Equal(t, *dbmc.Frequency, *apimc.Frequency)
	assert.Equal(t, *dbmc.Capacity, *apimc.Capacity)
	assert.Equal(t, *dbmc.Vendor, *apimc.Vendor)
	assert.Equal(t, *dbmc.Count, *apimc.Count)
}

func TestAPIMachineCapabilities_Validate(t *testing.T) {
	dpu := cdbm.MachineCapabilityDeviceTypeDPU
	spectrumX := cdbm.MachineCapabilityDeviceTypeSpectrumX
	tests := []struct {
		name               string
		caps               APIMachineCapabilities
		wantErrContains    string
		wantErrNotContains string
	}{
		{
			name: "same type and name with distinct device types",
			caps: APIMachineCapabilities{
				{Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8", DeviceType: &dpu},
				{Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8", DeviceType: &spectrumX},
			},
		},
		{
			name: "duplicate full identity",
			caps: APIMachineCapabilities{
				{Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8", DeviceType: &spectrumX},
				{Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8", DeviceType: &spectrumX},
			},
			wantErrContains: "duplicate Capability name and device type: ConnectX-8, SpectrumX",
		},
		{
			name: "duplicate identity without device type",
			caps: APIMachineCapabilities{
				{Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8"},
				{Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8"},
			},
			wantErrContains:    "duplicate Capability name: ConnectX-8",
			wantErrNotContains: "device type",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.caps.Validate()
			if tt.wantErrContains == "" {
				assert.NoError(t, err)
				return
			}
			if assert.Error(t, err) {
				assert.ErrorContains(t, err, tt.wantErrContains)
				if tt.wantErrNotContains != "" {
					assert.NotContains(t, err.Error(), tt.wantErrNotContains)
				}
			}
		})
	}
}
