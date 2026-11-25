// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestNewAPIMachineInterface(t *testing.T) {
	populated := &cdbm.MachineInterface{
		ID:                    uuid.New(),
		MachineID:             uuid.NewString(),
		ControllerInterfaceID: cutil.GetPtr(uuid.New()),
		ControllerSegmentID:   cutil.GetPtr(uuid.New()),
		AttachedDPUMachineID:  cutil.GetPtr(uuid.NewString()),
		Hostname:              cutil.GetPtr("test.com"),
		IsPrimary:             true,
		SubnetID:              cutil.GetPtr(uuid.New()),
		MacAddress:            cutil.GetPtr("00:00:00:00:00:00"),
		IPAddresses:           []string{"192.168.0.1, 172.168.0.1"},
	}

	tests := []struct {
		name              string
		dbmi              *cdbm.MachineInterface
		isProviderOrAdmin bool
		check             func(t *testing.T, got *APIMachineInterface)
	}{
		{
			name:              "populates all interface fields",
			dbmi:              populated,
			isProviderOrAdmin: true,
			check: func(t *testing.T, got *APIMachineInterface) {
				assert.Equal(t, populated.ID.String(), got.ID)
				assert.Equal(t, populated.MachineID, got.MachineID)
				assert.Equal(t, populated.ControllerInterfaceID.String(), *got.ControllerInterfaceID)
				assert.Equal(t, populated.ControllerSegmentID.String(), *got.ControllerSegmentID)
				assert.Equal(t, *populated.AttachedDPUMachineID, *got.AttachedDPUMachineID)
				assert.Equal(t, populated.SubnetID.String(), *got.SubnetID)
				assert.Equal(t, *populated.Hostname, *got.Hostname)
				assert.Equal(t, populated.IsPrimary, got.IsPrimary)
				assert.Equal(t, *populated.MacAddress, *got.MacAddress)
				assert.Equal(t, populated.IPAddresses, got.IPAddresses)
			},
		},
		{
			name:              "nil IPAddresses initializes to empty slice",
			dbmi:              &cdbm.MachineInterface{ID: uuid.New(), MachineID: "m-1"},
			isProviderOrAdmin: false,
			check: func(t *testing.T, got *APIMachineInterface) {
				assert.Equal(t, []string{}, got.IPAddresses)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIMachineInterface(tt.dbmi, tt.isProviderOrAdmin)
			tt.check(t, got)
		})
	}
}
