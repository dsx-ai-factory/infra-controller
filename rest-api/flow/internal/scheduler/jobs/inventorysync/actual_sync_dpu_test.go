// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestDesiredDpuBMCs(t *testing.T) {
	componentA := model.Component{ID: uuid.MustParse("00000000-0000-0000-0000-000000000001"), ComponentID: strPtr("host-1")}
	componentB := model.Component{ID: uuid.MustParse("00000000-0000-0000-0000-000000000002"), ComponentID: strPtr("host-2")}

	t.Run("multiple DPUs are projected by MAC", func(t *testing.T) {
		details := []nicoapi.MachineDetail{
			{
				MachineID:               "host-1",
				MachineType:             corev1.MachineType_HOST.String(),
				AssociatedDpuMachineIDs: []string{"dpu-1", "dpu-2"},
			},
			{MachineID: "dpu-1", MachineType: corev1.MachineType_DPU.String(), BmcMac: "AA-BB-CC-DD-EE-01", BmcIP: "10.0.0.1"},
			{MachineID: "dpu-2", MachineType: corev1.MachineType_DPU.String(), BmcMac: "aa:bb:cc:dd:ee:02"},
		}

		desired, err := desiredDpuBMCs(details, []model.Component{componentA})

		require.NoError(t, err)
		require.Len(t, desired, 2)
		byMAC := make(map[string]model.BMC, len(desired))
		for _, bmc := range desired {
			byMAC[bmc.MacAddress] = bmc
		}
		assert.Equal(t, componentA.ID, byMAC["aa:bb:cc:dd:ee:01"].ComponentID)
		assert.Equal(t, "10.0.0.1", *byMAC["aa:bb:cc:dd:ee:01"].IPAddress)
		assert.Nil(t, byMAC["aa:bb:cc:dd:ee:02"].IPAddress)
	})

	t.Run("unrepresented Core host is not projected", func(t *testing.T) {
		details := []nicoapi.MachineDetail{
			{MachineID: "host-only-in-core", MachineType: corev1.MachineType_HOST.String(), AssociatedDpuMachineIDs: []string{"dpu-1"}},
			{MachineID: "dpu-1", MachineType: corev1.MachineType_DPU.String(), BmcMac: "invalid"},
		}

		desired, err := desiredDpuBMCs(details, []model.Component{componentA})

		require.NoError(t, err)
		assert.Empty(t, desired)
	})

	testCases := []struct {
		name       string
		details    []nicoapi.MachineDetail
		components []model.Component
		errorText  string
	}{
		{
			name: "associated DPU missing from snapshot",
			details: []nicoapi.MachineDetail{
				{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String(), AssociatedDpuMachineIDs: []string{"missing"}},
			},
			components: []model.Component{componentA},
			errorText:  "missing from the machine snapshot",
		},
		{
			name: "associated machine has wrong type",
			details: []nicoapi.MachineDetail{
				{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String(), AssociatedDpuMachineIDs: []string{"not-dpu"}},
				{MachineID: "not-dpu", MachineType: corev1.MachineType_HOST.String()},
			},
			components: []model.Component{componentA},
			errorText:  "expected DPU",
		},
		{
			name: "same DPU repeated by one Core host",
			details: []nicoapi.MachineDetail{
				{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String(), AssociatedDpuMachineIDs: []string{"dpu-1", "dpu-1"}},
				{MachineID: "dpu-1", MachineType: corev1.MachineType_DPU.String(), BmcMac: "aa:bb:cc:dd:ee:01"},
			},
			components: []model.Component{componentA},
			errorText:  "contains duplicate association",
		},
		{
			name: "DPU has two Core hosts",
			details: []nicoapi.MachineDetail{
				{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String(), AssociatedDpuMachineIDs: []string{"dpu-1"}},
				{MachineID: "host-2", MachineType: corev1.MachineType_HOST.String(), AssociatedDpuMachineIDs: []string{"dpu-1"}},
				{MachineID: "dpu-1", MachineType: corev1.MachineType_DPU.String(), BmcMac: "aa:bb:cc:dd:ee:01"},
			},
			components: []model.Component{componentA, componentB},
			errorText:  "associated with multiple hosts",
		},
		{
			name: "associated DPU has no valid BMC MAC",
			details: []nicoapi.MachineDetail{
				{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String(), AssociatedDpuMachineIDs: []string{"dpu-1"}},
				{MachineID: "dpu-1", MachineType: corev1.MachineType_DPU.String()},
			},
			components: []model.Component{componentA},
			errorText:  "invalid BMC MAC address",
		},
		{
			name: "duplicate Flow host identity",
			details: []nicoapi.MachineDetail{
				{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String()},
			},
			components: []model.Component{componentA, {ID: componentB.ID, ComponentID: strPtr("host-1")}},
			errorText:  "share Core host machine ID",
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := desiredDpuBMCs(tc.details, tc.components)
			require.ErrorContains(t, err, tc.errorText)
		})
	}
}

func TestPlanDpuBMCReconciliation(t *testing.T) {
	componentA := uuid.MustParse("00000000-0000-0000-0000-000000000001")
	componentB := uuid.MustParse("00000000-0000-0000-0000-000000000002")
	dpuType := devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU)
	hostType := devicetypes.BMCTypeToString(devicetypes.BMCTypeHost)

	t.Run("creates moves updates and removes by MAC", func(t *testing.T) {
		desired := []model.BMC{
			{MacAddress: "aa:bb:cc:dd:ee:01", Type: dpuType, ComponentID: componentB, IPAddress: strPtr("10.0.0.11")},
			{MacAddress: "aa:bb:cc:dd:ee:02", Type: dpuType, ComponentID: componentA},
		}
		existing := []model.BMC{
			{MacAddress: "AA-BB-CC-DD-EE-01", Type: dpuType, ComponentID: componentA, IPAddress: strPtr("10.0.0.1"), User: strPtr("admin")},
			{MacAddress: "aa:bb:cc:dd:ee:03", Type: dpuType, ComponentID: componentA},
		}

		plan, err := planDpuBMCReconciliation(desired, existing)

		require.NoError(t, err)
		require.Len(t, plan.inserts, 1)
		assert.Equal(t, "aa:bb:cc:dd:ee:02", plan.inserts[0].MacAddress)
		require.Len(t, plan.updates, 1)
		assert.Equal(t, "AA-BB-CC-DD-EE-01", plan.updates[0].MacAddress)
		assert.Equal(t, componentB, plan.updates[0].ComponentID)
		assert.Equal(t, "10.0.0.11", *plan.updates[0].IPAddress)
		assert.Equal(t, "admin", *plan.updates[0].User, "credentials are preserved")
		require.Len(t, plan.deletes, 1)
		assert.Equal(t, "aa:bb:cc:dd:ee:03", plan.deletes[0].MacAddress)
	})

	t.Run("primary BMC MAC collision fails the complete plan", func(t *testing.T) {
		desired := []model.BMC{{MacAddress: "aa:bb:cc:dd:ee:01", Type: dpuType, ComponentID: componentA}}
		existing := []model.BMC{{MacAddress: "aa:bb:cc:dd:ee:01", Type: hostType, ComponentID: componentA}}

		_, err := planDpuBMCReconciliation(desired, existing)

		require.ErrorContains(t, err, "occupied by Flow BMC type")
	})

	t.Run("invalid stored DPU MAC is scheduled for deletion", func(t *testing.T) {
		existing := []model.BMC{{MacAddress: "invalid", Type: dpuType, ComponentID: componentA}}

		plan, err := planDpuBMCReconciliation(nil, existing)

		require.NoError(t, err)
		require.Len(t, plan.deletes, 1)
		assert.Equal(t, "invalid", plan.deletes[0].MacAddress)
	})

	t.Run("duplicate normalized stored MAC fails the complete plan", func(t *testing.T) {
		existing := []model.BMC{
			{MacAddress: "AA-BB-CC-DD-EE-01", Type: dpuType, ComponentID: componentA},
			{MacAddress: "aa:bb:cc:dd:ee:01", Type: dpuType, ComponentID: componentB},
		}

		_, err := planDpuBMCReconciliation(nil, existing)

		require.ErrorContains(t, err, "normalize to the same MAC")
	})
}

func TestReconcileDpuBMCs(t *testing.T) {
	t.Run("reconciles BMCs by MAC", testReconcileDpuBMCsByMAC)
	t.Run("invalid snapshot preserves rows", testReconcileDpuBMCsInvalidSnapshotPreservesRows)
}

func testReconcileDpuBMCsByMAC(t *testing.T) {
	ctx, pool := mirrorTestPool(t)
	dpuType := devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU)

	componentA := model.Component{Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute), ComponentID: strPtr("host-1")}
	componentB := model.Component{Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute), ComponentID: strPtr("host-2")}
	require.NoError(t, componentA.Create(ctx, pool.DB))
	require.NoError(t, componentB.Create(ctx, pool.DB))

	legacy := model.BMC{
		MacAddress:  "aa:bb:cc:dd:ee:01",
		Type:        dpuType,
		ComponentID: componentA.ID,
		IPAddress:   strPtr("10.0.0.1"),
		User:        strPtr("admin"),
		Password:    strPtr("secret"),
	}
	stale := model.BMC{MacAddress: "aa:bb:cc:dd:ee:03", Type: dpuType, ComponentID: componentA.ID}
	_, err := pool.DB.NewInsert().Model(&[]model.BMC{legacy, stale}).Exec(ctx)
	require.NoError(t, err)

	details := []nicoapi.MachineDetail{
		{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String(), AssociatedDpuMachineIDs: []string{"dpu-1"}},
		{MachineID: "host-2", MachineType: corev1.MachineType_HOST.String(), AssociatedDpuMachineIDs: []string{"dpu-2"}},
		{MachineID: "dpu-1", MachineType: corev1.MachineType_DPU.String(), BmcMac: legacy.MacAddress, BmcIP: "10.0.0.11"},
		{MachineID: "dpu-2", MachineType: corev1.MachineType_DPU.String(), BmcMac: "aa:bb:cc:dd:ee:02", BmcIP: "10.0.0.12"},
	}
	components := []model.Component{componentA, componentB}

	require.NoError(t, reconcileDpuBMCs(ctx, pool, details, components))
	// A second pass proves restart/idempotence relies only on the persisted MAC.
	require.NoError(t, reconcileDpuBMCs(ctx, pool, details, components))

	var got []model.BMC
	require.NoError(t, pool.DB.NewSelect().Model(&got).Order("mac_address ASC").Scan(ctx))
	require.Len(t, got, 2)
	assert.Equal(t, legacy.MacAddress, got[0].MacAddress)
	assert.Equal(t, componentA.ID, got[0].ComponentID)
	assert.Equal(t, "10.0.0.11", *got[0].IPAddress)
	assert.Equal(t, "admin", *got[0].User)
	assert.Equal(t, "secret", *got[0].Password)
	assert.Equal(t, componentB.ID, got[1].ComponentID)

	// Core reassociation moves the same MAC to the new host without any stored
	// DPU machine ID.
	details[0].AssociatedDpuMachineIDs = nil
	details[1].AssociatedDpuMachineIDs = []string{"dpu-1", "dpu-2"}
	require.NoError(t, reconcileDpuBMCs(ctx, pool, details, components))

	var moved model.BMC
	require.NoError(t, pool.DB.NewSelect().Model(&moved).Where("mac_address = ?", legacy.MacAddress).Scan(ctx))
	assert.Equal(t, componentB.ID, moved.ComponentID)
}

func testReconcileDpuBMCsInvalidSnapshotPreservesRows(t *testing.T) {
	ctx, pool := mirrorTestPool(t)
	component := model.Component{Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute), ComponentID: strPtr("host-1")}
	require.NoError(t, component.Create(ctx, pool.DB))
	existing := model.BMC{
		MacAddress:  "aa:bb:cc:dd:ee:01",
		Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU),
		ComponentID: component.ID,
	}
	_, err := pool.DB.NewInsert().Model(&existing).Exec(ctx)
	require.NoError(t, err)

	err = reconcileDpuBMCs(ctx, pool, []nicoapi.MachineDetail{
		{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String(), AssociatedDpuMachineIDs: []string{"missing-dpu"}},
	}, []model.Component{component})

	require.ErrorContains(t, err, "missing from the machine snapshot")
	var got []model.BMC
	require.NoError(t, pool.DB.NewSelect().Model(&got).Scan(ctx))
	require.Len(t, got, 1)
	assert.Equal(t, existing.MacAddress, got[0].MacAddress)
}
