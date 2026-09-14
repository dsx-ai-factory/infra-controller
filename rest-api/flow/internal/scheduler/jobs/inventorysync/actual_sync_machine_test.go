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

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
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

type actualInventoryTestClient struct {
	nicoapi.Client
	machines           []nicoapi.MachineDetail
	switches           []nicoapi.ObservedControllerDevice
	powerShelves       []nicoapi.ObservedControllerDevice
	machineErr         error
	switchErr          error
	powerShelfErr      error
	machinePositionErr error
}

func (c *actualInventoryTestClient) GetMachines(_ context.Context) ([]nicoapi.MachineDetail, error) {
	return c.machines, c.machineErr
}

func (c *actualInventoryTestClient) GetSwitches(_ context.Context) ([]nicoapi.ObservedControllerDevice, error) {
	return c.switches, c.switchErr
}

func (c *actualInventoryTestClient) GetPowerShelves(_ context.Context) ([]nicoapi.ObservedControllerDevice, error) {
	return c.powerShelves, c.powerShelfErr
}

func (c *actualInventoryTestClient) GetMachinePositionInfo(
	_ context.Context,
	_ []string,
) ([]nicoapi.MachinePosition, error) {
	return nil, c.machinePositionErr
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

	t.Run("links host and reconciles associated DPU in one cycle", func(t *testing.T) {
		ctx, pool := mirrorTestPool(t)
		const hostMAC = "aa:bb:cc:dd:ee:10"
		const dpuMAC = "aa:bb:cc:dd:ee:11"

		component := model.Component{Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute)}
		require.NoError(t, component.Create(ctx, pool.DB))
		createTestBMC(ctx, t, pool, component.ID, hostMAC)

		client := nicoapi.NewMockClient()
		client.AddMachine(nicoapi.MachineDetail{
			MachineID:               "host-1",
			MachineType:             corev1.MachineType_HOST.String(),
			BmcMac:                  hostMAC,
			AssociatedDpuMachineIDs: []string{"dpu-1"},
		})
		client.AddMachine(nicoapi.MachineDetail{
			MachineID:   "dpu-1",
			MachineType: corev1.MachineType_DPU.String(),
			BmcMac:      dpuMAC,
			BmcIP:       "10.0.0.11",
		})

		_, drifts, ok := syncMachines(ctx, pool, client)

		require.True(t, ok)
		assert.Empty(t, drifts)
		var persistedComponent model.Component
		require.NoError(t, pool.DB.NewSelect().Model(&persistedComponent).Where("id = ?", component.ID).Scan(ctx))
		require.NotNil(t, persistedComponent.ComponentID)
		assert.Equal(t, "host-1", *persistedComponent.ComponentID)
		var dpu model.BMC
		require.NoError(t, pool.DB.NewSelect().Model(&dpu).Where("mac_address = ?", dpuMAC).Scan(ctx))
		assert.Equal(t, devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU), dpu.Type)
		assert.Equal(t, component.ID, dpu.ComponentID)
		assert.Equal(t, "10.0.0.11", *dpu.IPAddress)
	})

	t.Run("GetMachines failure preserves DPU inventory", func(t *testing.T) {
		ctx, pool := mirrorTestPool(t)
		component := model.Component{Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute)}
		require.NoError(t, component.Create(ctx, pool.DB))
		existing := model.BMC{
			MacAddress:  "aa:bb:cc:dd:ee:11",
			Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU),
			ComponentID: component.ID,
		}
		_, err := pool.DB.NewInsert().Model(&existing).Exec(ctx)
		require.NoError(t, err)

		_, _, ok := syncMachines(ctx, pool, &failGetMachinesClient{Client: nicoapi.NewMockClient()})

		assert.False(t, ok)
		var got []model.BMC
		require.NoError(t, pool.DB.NewSelect().Model(&got).Scan(ctx))
		require.Len(t, got, 1)
		assert.Equal(t, existing.MacAddress, got[0].MacAddress)
	})

	t.Run("DPU failure does not block host convergence", func(t *testing.T) {
		ctx, pool := mirrorTestPool(t)
		component := model.Component{
			Type:        devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
			ComponentID: strPtr("host-1"),
		}
		require.NoError(t, component.Create(ctx, pool.DB))
		createTestBMC(ctx, t, pool, component.ID, "aa:bb:cc:dd:ee:10")

		client := nicoapi.NewMockClient()
		client.AddMachine(nicoapi.MachineDetail{
			MachineID:               "host-1",
			MachineType:             corev1.MachineType_HOST.String(),
			BmcMac:                  "aa:bb:cc:dd:ee:10",
			AssociatedDpuMachineIDs: []string{"missing-dpu"},
		})
		client.AddPowerState("host-1", nicoapi.PowerStateOn)

		_, _, ok := syncMachines(ctx, pool, client)

		assert.False(t, ok, "the cycle remains degraded when DPU reconciliation fails")
		var persisted model.Component
		require.NoError(t, pool.DB.NewSelect().Model(&persisted).Where("id = ?", component.ID).Scan(ctx))
		require.NotNil(t, persisted.PowerState)
		assert.Equal(t, nicoapi.PowerStateOn, *persisted.PowerState)
	})

	t.Run("position failure does not block DPU reconciliation", func(t *testing.T) {
		ctx, pool := mirrorTestPool(t)
		const hostMAC = "aa:bb:cc:dd:ee:20"
		const dpuMAC = "aa:bb:cc:dd:ee:21"
		component := model.Component{
			Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
		}
		require.NoError(t, component.Create(ctx, pool.DB))
		createTestBMC(ctx, t, pool, component.ID, hostMAC)

		client := &actualInventoryTestClient{
			Client: nicoapi.NewMockClient(),
			machines: []nicoapi.MachineDetail{
				{
					MachineID:               "host-1",
					MachineType:             corev1.MachineType_HOST.String(),
					BmcMac:                  hostMAC,
					AssociatedDpuMachineIDs: []string{"dpu-1"},
				},
				{
					MachineID:   "dpu-1",
					MachineType: corev1.MachineType_DPU.String(),
					BmcMac:      dpuMAC,
				},
			},
			machinePositionErr: errors.New("positions unavailable"),
		}

		_, _, ok := syncMachines(ctx, pool, client)

		assert.False(t, ok)
		var dpu model.BMC
		require.NoError(t, pool.DB.NewSelect().Model(&dpu).Where("mac_address = ?", dpuMAC).Scan(ctx))
		assert.Equal(t, devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU), dpu.Type)
		assert.Equal(t, component.ID, dpu.ComponentID)
	})
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

	for _, failedType := range []devicetypes.ComponentType{
		devicetypes.ComponentTypeCompute,
		devicetypes.ComponentTypeNVSwitch,
		devicetypes.ComponentTypePowerShelf,
	} {
		failedType := failedType
		t.Run("type failure preserves only "+devicetypes.ComponentTypeToString(failedType), func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			seedInventoryDrifts(t, ctx, pool, "old")

			client := newActualInventoryTestClient("first")
			switch failedType {
			case devicetypes.ComponentTypeCompute:
				client.machineErr = errors.New("compute unavailable")
			case devicetypes.ComponentTypeNVSwitch:
				client.switchErr = errors.New("switch unavailable")
			case devicetypes.ComponentTypePowerShelf:
				client.powerShelfErr = errors.New("power shelf unavailable")
			}

			runInventoryOne(ctx, pool, client, false)

			expected := map[string]string{
				"Compute":    "compute-first",
				"NVSwitch":   "switch-first",
				"PowerShelf": "powershelf-first",
			}
			expected[devicetypes.ComponentTypeToString(failedType)] = "old-" + devicetypes.ComponentTypeToString(failedType)
			assertInventoryDrifts(t, ctx, pool, expected)

			client = newActualInventoryTestClient("recovery")
			runInventoryOne(ctx, pool, client, false)
			assertInventoryDrifts(t, ctx, pool, map[string]string{
				"Compute":    "compute-recovery",
				"NVSwitch":   "switch-recovery",
				"PowerShelf": "powershelf-recovery",
			})
		})
	}

	t.Run("persistence failure does not block later component types", func(t *testing.T) {
		ctx, pool := mirrorTestPool(t)
		seedInventoryDrifts(t, ctx, pool, "old")
		_, err := pool.DB.ExecContext(ctx, `
			CREATE FUNCTION fail_nvswitch_drift_delete() RETURNS trigger AS $$
			BEGIN
				RAISE EXCEPTION 'injected NVSwitch persistence failure';
			END;
			$$ LANGUAGE plpgsql;
			CREATE TRIGGER fail_nvswitch_drift_delete
			BEFORE DELETE ON component_drift
			FOR EACH ROW WHEN (OLD.component_type = 'NVSwitch')
			EXECUTE FUNCTION fail_nvswitch_drift_delete();
		`)
		require.NoError(t, err)

		runInventoryOne(ctx, pool, newActualInventoryTestClient("first"), false)
		assertInventoryDrifts(t, ctx, pool, map[string]string{
			"Compute":    "compute-first",
			"NVSwitch":   "old-NVSwitch",
			"PowerShelf": "powershelf-first",
		})

		_, err = pool.DB.ExecContext(ctx, `
			DROP TRIGGER fail_nvswitch_drift_delete ON component_drift;
			DROP FUNCTION fail_nvswitch_drift_delete();
		`)
		require.NoError(t, err)
		runInventoryOne(ctx, pool, newActualInventoryTestClient("recovery"), false)
		assertInventoryDrifts(t, ctx, pool, map[string]string{
			"Compute":    "compute-recovery",
			"NVSwitch":   "switch-recovery",
			"PowerShelf": "powershelf-recovery",
		})
	})
}

func newActualInventoryTestClient(suffix string) *actualInventoryTestClient {
	return &actualInventoryTestClient{
		Client: nicoapi.NewMockClient(),
		machines: []nicoapi.MachineDetail{{
			MachineID:   "compute-" + suffix,
			MachineType: corev1.MachineType_HOST.String(),
		}},
		switches: []nicoapi.ObservedControllerDevice{{
			ID:     "switch-" + suffix,
			BmcMac: "aa:bb:cc:dd:ee:01",
		}},
		powerShelves: []nicoapi.ObservedControllerDevice{{
			ID:     "powershelf-" + suffix,
			BmcMac: "aa:bb:cc:dd:ee:02",
		}},
	}
}

func seedInventoryDrifts(t *testing.T, ctx context.Context, pool *cdb.Session, suffix string) {
	t.Helper()
	drifts := make([]model.ComponentDrift, 0, 3)
	for _, componentType := range []devicetypes.ComponentType{
		devicetypes.ComponentTypeCompute,
		devicetypes.ComponentTypeNVSwitch,
		devicetypes.ComponentTypePowerShelf,
	} {
		typeName := devicetypes.ComponentTypeToString(componentType)
		externalID := suffix + "-" + typeName
		drifts = append(drifts, model.ComponentDrift{
			ExternalID:    &externalID,
			ComponentType: &typeName,
			DriftType:     model.DriftTypeMissingInExpected,
			Diffs:         []model.FieldDiff{},
			CheckedAt:     time.Now(),
		})
	}
	_, err := pool.DB.NewInsert().Model(&drifts).Exec(ctx)
	require.NoError(t, err)
}

func assertInventoryDrifts(t *testing.T, ctx context.Context, pool *cdb.Session, expected map[string]string) {
	t.Helper()
	drifts, err := model.GetAllDrifts(ctx, pool.DB)
	require.NoError(t, err)
	require.Len(t, drifts, len(expected))
	for _, drift := range drifts {
		require.NotNil(t, drift.ComponentType)
		require.NotNil(t, drift.ExternalID)
		assert.Equal(t, expected[*drift.ComponentType], *drift.ExternalID)
	}
}

func TestCompareMachineFieldsForDrift(t *testing.T) {
	t.Run("no mismatch", testCompareMachineFieldsForDriftNoMismatch)
	t.Run("all positional fields mismatch", testCompareMachineFieldsForDriftAllPositionalFieldsMismatch)
	t.Run("nil position fields skipped", testCompareMachineFieldsForDriftNilPositionFieldsSkipped)
	t.Run("serial never compared", testCompareMachineFieldsForDriftSerialNeverCompared)
	t.Run("partial mismatch", testCompareMachineFieldsForDriftPartialMismatch)
	t.Run("missing position reports drift", testCompareMachineFieldsForDriftMissingPositionReportsDrift)
	t.Run("missing position with zero expected does not drift", testCompareMachineFieldsForDriftMissingPositionZeroExpectedNoDrift)
}

func testCompareMachineFieldsForDriftNoMismatch(t *testing.T) {
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

func testCompareMachineFieldsForDriftAllPositionalFieldsMismatch(t *testing.T) {
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

func testCompareMachineFieldsForDriftNilPositionFieldsSkipped(t *testing.T) {
	expected := &model.Component{
		SerialNumber:    "SN001",
		FirmwareVersion: "1.0.0",
		SlotID:          2,
		TrayIndex:       1,
		HostID:          5,
	}
	// Position found but all fields nil — should not produce diffs
	position := nicoapi.MachinePosition{}

	diffs := compareMachineFieldsForDrift(expected, &position)
	assert.Empty(t, diffs)
}

func testCompareMachineFieldsForDriftSerialNeverCompared(t *testing.T) {
	// Even when serial numbers differ, no drift is produced: serial is not a
	// correlation/drift signal anymore.
	expected := &model.Component{
		SerialNumber: "SN001",
	}
	position := nicoapi.MachinePosition{}

	diffs := compareMachineFieldsForDrift(expected, &position)
	assert.Empty(t, diffs)
}

func testCompareMachineFieldsForDriftPartialMismatch(t *testing.T) {
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

func testCompareMachineFieldsForDriftMissingPositionReportsDrift(t *testing.T) {
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

func testCompareMachineFieldsForDriftMissingPositionZeroExpectedNoDrift(t *testing.T) {
	expected := &model.Component{
		SerialNumber: "SN001",
		SlotID:       0,
		TrayIndex:    0,
		HostID:       0,
	}

	// nil position with zero-value expected fields — no position drift
	diffs := compareMachineFieldsForDrift(expected, nil)
	assert.Empty(t, diffs)
}
