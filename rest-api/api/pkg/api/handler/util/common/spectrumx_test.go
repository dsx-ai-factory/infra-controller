// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestGetSpectrumXCapabilitiesForMachines(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()
	TestSetupSchema(t, dbSession)
	user := TestBuildUser(t, dbSession, uuid.NewString(), "spectrumx-provider", nil)
	provider := TestBuildInfrastructureProvider(t, dbSession, "spectrumx-provider", "spectrumx-provider", user)
	site := TestBuildSite(t, dbSession, provider, "spectrumx-site", user)
	instanceType := TestBuildInstanceType(t, dbSession, "spectrumx-type", nil, site, nil, user)
	machineA := TestBuildMachine(t, dbSession, provider, site, nil, nil, cdbm.MachineStatusReady)
	machineB := TestBuildMachine(t, dbSession, provider, site, nil, nil, cdbm.MachineStatusReady)
	unrequested := TestBuildMachine(t, dbSession, provider, site, nil, nil, cdbm.MachineStatusReady)
	tx, err := cdb.BeginTx(ctx, dbSession, nil)
	require.NoError(t, err)
	defer func() { require.NoError(t, tx.Rollback()) }()

	// Uncommitted rows prove the query uses the caller's allocation transaction.
	// Other machines, Instance Type summaries and same-name DPU capabilities
	// must not contribute to a selected machine's SpectrumX eligibility.
	for _, input := range []cdbm.MachineCapabilityCreateInput{
		{MachineID: &machineA.ID, Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8", Count: cutil.GetPtr(2), DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeSpectrumX)},
		{MachineID: &machineB.ID, Type: cdbm.MachineCapabilityTypeNetwork, Name: "BlueField-3", Count: cutil.GetPtr(1), DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeSpectrumX)},
		{MachineID: &machineA.ID, Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8", Count: cutil.GetPtr(8), DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU)},
		{MachineID: &unrequested.ID, Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8", Count: cutil.GetPtr(8), DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeSpectrumX)},
		{InstanceTypeID: &instanceType.ID, Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8", Count: cutil.GetPtr(8), DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeSpectrumX)},
	} {
		_, err = cdbm.NewMachineCapabilityDAO(dbSession).Create(ctx, tx, input)
		require.NoError(t, err)
	}
	for _, test := range []struct {
		name       string
		machineIDs []string
		wantCounts map[string]int
	}{
		{name: "empty scope is not an unfiltered inventory query", wantCounts: map[string]int{}},
		{name: "scoped capabilities stay grouped by machine", machineIDs: []string{machineA.ID, machineB.ID, "missing"}, wantCounts: map[string]int{machineA.ID: 2, machineB.ID: 1}},
	} {
		t.Run(test.name, func(t *testing.T) {
			capabilities, readErr := GetSpectrumXCapabilitiesForMachines(ctx, tx, dbSession, test.machineIDs)
			require.NoError(t, readErr)
			counts := map[string]int{}
			for id, caps := range capabilities {
				require.Len(t, caps, 1)
				require.NotNil(t, caps[0].Count)
				counts[id] = *caps[0].Count
			}
			assert.Equal(t, test.wantCounts, counts)
		})
	}
}
