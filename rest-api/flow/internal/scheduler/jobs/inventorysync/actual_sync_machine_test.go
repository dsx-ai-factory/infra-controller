// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ptr is a generic helper that returns a pointer to the given value.
// Useful for constructing test structs with pointer fields (e.g. *int32, *string).
func ptr[T any](v T) *T { return &v }

// failGetMachinesClient wraps a working mock client and fails GetMachines.
type failGetMachinesClient struct {
	nicoapi.Client
}

// GetMachines makes failGetMachinesClient fail only the actual-inventory query
// while its embedded mock client continues to serve the other sync RPCs.
func (c *failGetMachinesClient) GetMachines(_ context.Context) ([]nicoapi.MachineDetail, error) {
	return nil, errors.New("boom")
}

func TestFilterHostMachineDetails(t *testing.T) {
	testCases := []struct {
		name               string
		machineDetails     []nicoapi.MachineDetail
		expectedMachineIDs []string
	}{
		{
			name:               "no machines",
			expectedMachineIDs: []string{},
		},
		{
			name: "host machines",
			machineDetails: []nicoapi.MachineDetail{
				{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String()},
				{MachineID: "host-2", MachineType: corev1.MachineType_HOST.String()},
			},
			expectedMachineIDs: []string{"host-1", "host-2"},
		},
		{
			name: "mixed host and DPU machines",
			machineDetails: []nicoapi.MachineDetail{
				{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String()},
				{MachineID: "dpu-1", MachineType: corev1.MachineType_DPU.String()},
				{MachineID: "host-2", MachineType: corev1.MachineType_HOST.String()},
			},
			expectedMachineIDs: []string{"host-1", "host-2"},
		},
		{
			name: "DPU machines",
			machineDetails: []nicoapi.MachineDetail{
				{MachineID: "dpu-1", MachineType: corev1.MachineType_DPU.String()},
			},
			expectedMachineIDs: []string{},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			hostMachineDetails := filterHostMachineDetails(tc.machineDetails)
			machineIDs := make([]string, 0, len(hostMachineDetails))
			for _, detail := range hostMachineDetails {
				machineIDs = append(machineIDs, detail.MachineID)
			}

			assert.Equal(t, tc.expectedMachineIDs, machineIDs)
		})
	}
}

func TestSyncMachines(t *testing.T) {
	testCases := []struct {
		name                       string
		machineDetails             []nicoapi.MachineDetail
		expectedHostBmcMac         string
		expectedDpuBmcMac          string
		expectedPersistedMachineID string
		expectedReceived           int
		expectedExternalIDs        []string
	}{
		{
			name:                "empty expected and actual inventory",
			expectedExternalIDs: []string{},
		},
		{
			name: "empty expected inventory with host and DPU actual inventory",
			machineDetails: []nicoapi.MachineDetail{
				{MachineID: "host-1", MachineType: corev1.MachineType_HOST.String()},
				{MachineID: "dpu-1", MachineType: corev1.MachineType_DPU.String()},
				{MachineID: "host-2", MachineType: corev1.MachineType_HOST.String()},
			},
			expectedReceived:    2,
			expectedExternalIDs: []string{"host-1", "host-2"},
		},
		{
			name: "matched expected host with orphan host and DPU actual inventory",
			machineDetails: []nicoapi.MachineDetail{
				{MachineID: "expected-host", MachineType: corev1.MachineType_HOST.String(), BmcMac: "aa:bb:cc:dd:ee:81"},
				{MachineID: "orphan-host", MachineType: corev1.MachineType_HOST.String()},
				{MachineID: "dpu-1", MachineType: corev1.MachineType_DPU.String(), BmcMac: "aa:bb:cc:dd:ee:82"},
			},
			expectedHostBmcMac:         "aa:bb:cc:dd:ee:81",
			expectedDpuBmcMac:          "aa:bb:cc:dd:ee:82",
			expectedPersistedMachineID: "expected-host",
			expectedReceived:           2,
			expectedExternalIDs:        []string{"orphan-host"},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			client := nicoapi.NewMockClient()
			for _, detail := range tc.machineDetails {
				client.AddMachine(detail)
			}
			var expectedComponent model.Component
			if tc.expectedHostBmcMac != "" {
				expectedComponent = model.Component{
					Type:         devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
					Manufacturer: "TestMfg",
					SerialNumber: "expected-host",
					SlotID:       unknownPositionValue,
					TrayIndex:    unknownPositionValue,
					HostID:       unknownPositionValue,
				}
				require.NoError(t, expectedComponent.Create(ctx, pool.DB))
				createTestBMC(ctx, t, pool, expectedComponent.ID, tc.expectedHostBmcMac)
			}
			if tc.expectedDpuBmcMac != "" {
				dpuBMC := model.BMC{
					MacAddress:  tc.expectedDpuBmcMac,
					ComponentID: expectedComponent.ID,
					Type:        "DPU",
				}
				_, err := pool.DB.NewInsert().Model(&dpuBMC).Exec(ctx)
				require.NoError(t, err)
			}

			received, drifts, rpcOK := syncMachines(ctx, pool, client)

			assert.True(t, rpcOK)
			assert.Equal(t, tc.expectedReceived, received)
			reportedExternalIDs := make([]string, 0, len(drifts))
			for _, drift := range drifts {
				assert.Equal(t, model.DriftTypeMissingInExpected, drift.DriftType)
				assert.Nil(t, drift.ComponentID)
				require.NotNil(t, drift.ExternalID)
				reportedExternalIDs = append(reportedExternalIDs, *drift.ExternalID)
			}
			assert.ElementsMatch(t, tc.expectedExternalIDs, reportedExternalIDs)

			if tc.expectedPersistedMachineID != "" {
				var persisted model.Component
				err := pool.DB.NewSelect().Model(&persisted).Where("id = ?", expectedComponent.ID).Scan(ctx)
				require.NoError(t, err)
				require.NotNil(t, persisted.ComponentID)
				assert.Equal(t, tc.expectedPersistedMachineID, *persisted.ComponentID)
			}
		})
	}
}

func TestRunInventoryOne(t *testing.T) {
	t.Run("final expected compute deletion keeps orphan host drift", func(t *testing.T) {
		ctx, pool := mirrorTestPool(t)
		client := nicoapi.NewMockClient()

		const hostMachineID = "host-after-expected-delete"
		const hostBmcMac = "aa:bb:cc:dd:ee:91"
		client.AddMachine(nicoapi.MachineDetail{
			MachineID:   hostMachineID,
			MachineType: corev1.MachineType_HOST.String(),
			BmcMac:      hostBmcMac,
		})

		component := model.Component{
			Type:         devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
			Manufacturer: "TestMfg",
			SerialNumber: "expected-host-to-delete",
			SlotID:       unknownPositionValue,
			TrayIndex:    unknownPositionValue,
			HostID:       unknownPositionValue,
		}
		require.NoError(t, component.Create(ctx, pool.DB))
		createTestBMC(ctx, t, pool, component.ID, hostBmcMac)

		runInventoryOne(ctx, pool, client, false)
		drifts, err := model.GetAllDrifts(ctx, pool.DB)
		require.NoError(t, err)
		assert.Empty(t, drifts)

		require.NoError(t, component.Delete(ctx, pool.DB))
		runInventoryOne(ctx, pool, client, false)

		drifts, err = model.GetAllDrifts(ctx, pool.DB)
		require.NoError(t, err)
		require.Len(t, drifts, 1)
		assert.Equal(t, model.DriftTypeMissingInExpected, drifts[0].DriftType)
		assert.Nil(t, drifts[0].ComponentID)
		require.NotNil(t, drifts[0].ExternalID)
		assert.Equal(t, hostMachineID, *drifts[0].ExternalID)
	})

	t.Run("Core query failure preserves prior orphan host drift", func(t *testing.T) {
		ctx, pool := mirrorTestPool(t)

		// With no expected compute rows, `syncMachines` still queries Core.
		// Keeping a reliable orphan-host result here proves that a failed call
		// remains different from a successful, empty actual inventory.
		externalID := "host-from-prior-cycle"
		existing := model.ComponentDrift{
			ExternalID: &externalID,
			DriftType:  model.DriftTypeMissingInExpected,
			Diffs:      []model.FieldDiff{},
			CheckedAt:  time.Now(),
		}
		_, err := pool.DB.NewInsert().Model(&existing).Exec(ctx)
		require.NoError(t, err)

		client := &failGetMachinesClient{Client: nicoapi.NewMockClient()}
		runInventoryOne(ctx, pool, client, false)

		drifts, err := model.GetAllDrifts(ctx, pool.DB)
		require.NoError(t, err)
		require.Len(t, drifts, 1, "drift table must not be wiped when an actual-sync RPC failed")
		require.NotNil(t, drifts[0].ExternalID)
		assert.Equal(t, externalID, *drifts[0].ExternalID)
	})
}

func TestCompareMachineFieldsForDrift_NoMismatch(t *testing.T) {
	expected := &model.Component{
		SerialNumber:    "SN001",
		FirmwareVersion: "1.0.0",
		SlotID:          2,
		TrayIndex:       1,
		HostID:          5,
	}
	position := nicoapi.MachinePosition{
		PhysicalSlotNum:  ptr(int32(2)),
		ComputeTrayIndex: ptr(int32(1)),
		TopologyID:       ptr(int32(5)),
	}

	diffs := compareMachineFieldsForDrift(expected, &position)
	assert.Empty(t, diffs)
}

func TestCompareMachineFieldsForDrift_AllPositionalFieldsMismatch(t *testing.T) {
	expected := &model.Component{
		SerialNumber:    "SN001",
		FirmwareVersion: "1.0.0",
		SlotID:          2,
		TrayIndex:       1,
		HostID:          5,
	}
	position := nicoapi.MachinePosition{
		PhysicalSlotNum:  ptr(int32(10)),
		ComputeTrayIndex: ptr(int32(3)),
		TopologyID:       ptr(int32(7)),
	}

	diffs := compareMachineFieldsForDrift(expected, &position)
	assert.Len(t, diffs, 3)

	diffByField := make(map[string]model.FieldDiff)
	for _, d := range diffs {
		diffByField[d.FieldName] = d
	}

	assert.Equal(t, "2", diffByField["slot_id"].ExpectedValue)
	assert.Equal(t, "10", diffByField["slot_id"].ActualValue)

	assert.Equal(t, "1", diffByField["tray_index"].ExpectedValue)
	assert.Equal(t, "3", diffByField["tray_index"].ActualValue)

	assert.Equal(t, "5", diffByField["host_id"].ExpectedValue)
	assert.Equal(t, "7", diffByField["host_id"].ActualValue)

	// Serial number is no longer a drift signal (correlation is by BMC MAC).
	assert.NotContains(t, diffByField, "serial_number")
	assert.NotContains(t, diffByField, "firmware_version")
}

func TestCompareMachineFieldsForDrift_NilPositionFieldsReportedMissing(t *testing.T) {
	expected := &model.Component{
		SerialNumber:    "SN001",
		FirmwareVersion: "1.0.0",
		SlotID:          2,
		TrayIndex:       1,
		HostID:          5,
	}
	// The position row exists, but each expected coordinate is still missing.
	position := nicoapi.MachinePosition{}

	diffs := compareMachineFieldsForDrift(expected, &position)
	require.Len(t, diffs, 3)
	for _, diff := range diffs {
		assert.Equal(t, "<missing>", diff.ActualValue)
	}
}

func TestCompareMachineFieldsForDrift_SerialNeverCompared(t *testing.T) {
	// Even when serial numbers differ, no drift is produced: serial is not a
	// correlation/drift signal anymore.
	expected := &model.Component{
		SerialNumber: "SN001",
		SlotID:       unknownPositionValue,
		TrayIndex:    unknownPositionValue,
		HostID:       unknownPositionValue,
	}
	position := nicoapi.MachinePosition{}

	diffs := compareMachineFieldsForDrift(expected, &position)
	assert.Empty(t, diffs)
}

func TestCompareMachineFieldsForDrift_PartialMismatch(t *testing.T) {
	expected := &model.Component{
		SerialNumber:    "SN001",
		FirmwareVersion: "1.0.0",
		SlotID:          2,
		TrayIndex:       1,
		HostID:          5,
	}
	position := nicoapi.MachinePosition{
		PhysicalSlotNum:  ptr(int32(2)), // match
		ComputeTrayIndex: ptr(int32(1)), // match
		TopologyID:       ptr(int32(9)), // mismatch
	}

	diffs := compareMachineFieldsForDrift(expected, &position)
	assert.Len(t, diffs, 1)

	diffByField := make(map[string]model.FieldDiff)
	for _, d := range diffs {
		diffByField[d.FieldName] = d
	}

	assert.NotContains(t, diffByField, "firmware_version")
	assert.Contains(t, diffByField, "host_id")
	assert.NotContains(t, diffByField, "slot_id")
	assert.NotContains(t, diffByField, "tray_index")
	assert.NotContains(t, diffByField, "serial_number")
}

func TestCompareMachineFieldsForDrift_MissingPositionReportsDrift(t *testing.T) {
	expected := &model.Component{
		SerialNumber:    "SN001",
		FirmwareVersion: "1.0.0",
		SlotID:          2,
		TrayIndex:       1,
		HostID:          5,
	}

	// nil position means no entry in positionByID — should flag non-zero expected fields
	diffs := compareMachineFieldsForDrift(expected, nil)
	assert.Len(t, diffs, 3)

	diffByField := make(map[string]model.FieldDiff)
	for _, d := range diffs {
		diffByField[d.FieldName] = d
	}

	assert.Equal(t, "2", diffByField["slot_id"].ExpectedValue)
	assert.Equal(t, "<missing>", diffByField["slot_id"].ActualValue)

	assert.Equal(t, "1", diffByField["tray_index"].ExpectedValue)
	assert.Equal(t, "<missing>", diffByField["tray_index"].ActualValue)

	assert.Equal(t, "5", diffByField["host_id"].ExpectedValue)
	assert.Equal(t, "<missing>", diffByField["host_id"].ActualValue)
}

func TestCompareMachineFieldsForDrift_MissingPositionExplicitZeroReportsDrift(t *testing.T) {
	expected := &model.Component{
		SerialNumber: "SN001",
		SlotID:       0,
		TrayIndex:    0,
		HostID:       0,
	}

	// Zero is an explicitly configured position, not an unknown sentinel.
	diffs := compareMachineFieldsForDrift(expected, nil)
	require.Len(t, diffs, 3)
	for _, diff := range diffs {
		assert.Equal(t, "0", diff.ExpectedValue)
		assert.Equal(t, "<missing>", diff.ActualValue)
	}
}

func TestCompareMachineFieldsForDrift_UnknownExpectedPositionSkipped(t *testing.T) {
	expected := &model.Component{
		SlotID:    unknownPositionValue,
		TrayIndex: unknownPositionValue,
		HostID:    unknownPositionValue,
	}

	assert.Empty(t, compareMachineFieldsForDrift(expected, nil))
	assert.Empty(t, compareMachineFieldsForDrift(expected, &nicoapi.MachinePosition{
		PhysicalSlotNum:  ptr(int32(0)),
		ComputeTrayIndex: ptr(int32(0)),
		TopologyID:       ptr(int32(0)),
	}))
}
