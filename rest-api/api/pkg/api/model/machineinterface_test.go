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

func TestMachineInterface_NewAPIMachineInterface(t *testing.T) {
	dbmi := &cdbm.MachineInterface{
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
	apimi := NewAPIMachineInterface(dbmi, true)
	assert.Equal(t, dbmi.ID.String(), apimi.ID)
	assert.Equal(t, dbmi.MachineID, apimi.MachineID)
	assert.Equal(t, dbmi.ControllerInterfaceID.String(), *apimi.ControllerInterfaceID)
	assert.Equal(t, dbmi.ControllerSegmentID.String(), *apimi.ControllerSegmentID)
	assert.Equal(t, *dbmi.AttachedDPUMachineID, *apimi.AttachedDPUMachineID)
	assert.Equal(t, dbmi.SubnetID.String(), *apimi.SubnetID)
	assert.Equal(t, *dbmi.Hostname, *apimi.Hostname)
	assert.Equal(t, dbmi.IsPrimary, apimi.IsPrimary)
	assert.Equal(t, *dbmi.MacAddress, *apimi.MacAddress)
	assert.Equal(t, len(dbmi.IPAddresses), len(apimi.IPAddresses))
	for i, v := range apimi.IPAddresses {
		assert.Equal(t, dbmi.IPAddresses[i], v)
	}
}
