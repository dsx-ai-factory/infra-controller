// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"errors"
	"fmt"
	"os"
	"testing"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// createTestBMC inserts a single BMC row for the given component so BMC-MAC
// linking has a MAC to correlate on.
func createTestBMC(ctx context.Context, t *testing.T, pool *cdb.Session, componentID uuid.UUID, mac string) {
	t.Helper()
	bmc := model.BMC{MacAddress: mac, ComponentID: componentID, Type: "Host"}
	_, err := pool.DB.NewInsert().Model(&bmc).Exec(ctx)
	assert.Nil(t, err)
}

type firmwareInventoryClient struct {
	nicoapi.Client
	response *corev1.GetComponentInventoryResponse
	err      error
	request  *corev1.GetComponentInventoryRequest
	before   func()
}

func (c *firmwareInventoryClient) GetComponentInventory(
	_ context.Context,
	req *corev1.GetComponentInventoryRequest,
) (*corev1.GetComponentInventoryResponse, error) {
	c.request = req
	if c.before != nil {
		c.before()
	}
	return c.response, c.err
}

// TestInventory is the main test for the inventory package
func TestInventory(t *testing.T) {
	ctx := context.Background()

	if os.Getenv("DB_PORT") == "" {
		log.Warn().Msgf("Not running unit test due to no DB environment specified")
		t.SkipNow()
	}

	dbConf, err := cdb.ConfigFromEnv()
	assert.Nil(t, err)
	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	assert.Nil(t, err)

	grpcMock := nicoapi.NewMockClient()

	// Create a basic faked GRPC environment. Linking is keyed on BMC MAC now
	// (matched against the machine's BmcMac), so machines no longer carry a
	// chassis serial for correlation.
	mac2 := "aa:bb:cc:dd:ee:02"
	mac4 := "aa:bb:cc:dd:ee:04"
	hostType := corev1.MachineType_HOST.String()
	grpcMock.AddMachine(nicoapi.MachineDetail{MachineID: "id1", MachineType: hostType})
	grpcMock.AddMachine(nicoapi.MachineDetail{MachineID: "id2", BmcMac: mac2, MachineType: hostType})
	grpcMock.AddMachine(nicoapi.MachineDetail{MachineID: "id3", MachineType: hostType})
	grpcMock.AddMachine(nicoapi.MachineDetail{MachineID: "id4", MachineType: hostType})
	grpcMock.AddPowerState("id2", nicoapi.PowerStateOn)

	// serial2's BMC MAC (mac2) matches machine id2; serial4's BMC MAC (mac4)
	// matches no machine, so it stays unmatched (missing_in_actual).

	// Create a rack (required for components due to NOT NULL constraint)
	rack := model.Rack{
		Name:         "test-rack",
		Manufacturer: "TestMfg",
		SerialNumber: "rack-serial-001",
	}
	err = rack.Create(ctx, pool.DB)
	assert.Nil(t, err)

	// Create components with required fields (manufacturer and rack_id are NOT NULL)
	c := model.Component{SerialNumber: "serial2", Manufacturer: "TestMfg", RackID: rack.ID}
	err = c.Create(ctx, pool.DB)
	assert.Nil(t, err)
	createTestBMC(ctx, t, pool, c.ID, mac2)
	c = model.Component{SerialNumber: "serial4", Manufacturer: "TestMfg2", RackID: rack.ID}
	err = c.Create(ctx, pool.DB)
	assert.Nil(t, err)
	createTestBMC(ctx, t, pool, c.ID, mac4)

	// expectedSyncEnabled=false: this test exercises actual-sync only. The
	// mock carries no expected machines, so running the mirror would treat
	// that as Core authoritatively reporting zero compute components and
	// soft-delete serial2/serial4 before actual-sync runs. The mirror has
	// its own coverage in expected_mirror_db_test.go.
	runInventoryOne(ctx, pool, grpcMock, false)

	rows, err := pool.DB.Query("SELECT serial_number, power_state FROM component;")
	assert.NotNil(t, rows)
	assert.Nil(t, err)
	defer rows.Close()

	var found int
	for rows.Next() {
		var serial string
		var state *nicoapi.PowerState
		require.NoError(t, rows.Scan(&serial, &state))

		switch serial {
		case "serial2":
			assert.Equal(t, *state, nicoapi.PowerStateOn)
			found++
		case "serial4":
			assert.Nil(t, state)
			found++
		default:
			panic(fmt.Sprintf("Invalid row found: %v %v", serial, state))
		}
	}
	assert.Equal(t, 2, found)
}

// TestSyncFirmwareVersion verifies that syncMachines sources compute firmware
// from component inventory while preserving the prior value when that
// best-effort inventory is unavailable.
func TestSyncFirmwareVersion(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		log.Warn().Msgf("Not running unit test due to no DB environment specified")
		t.SkipNow()
	}

	testCases := []struct {
		name                 string
		canonicalVersion     string
		inventoryID          string
		inventoryDescription string
		inventoryVersion     string
		inventoryStatus      corev1.ComponentManagerStatusCode
		inventoryErr         error
		omitReport           bool
		concurrentLeakUpdate bool
		expectedVersion      string
	}{
		{
			name:                 "canonical component inventory replaces the predecessor machine field",
			canonicalVersion:     "3.0.0",
			inventoryDescription: "BMC image",
			inventoryVersion:     "raw-version-must-not-win",
			expectedVersion:      "3.0.0",
		},
		{
			name:                 "exact BMC inventory ID is the raw fallback",
			inventoryID:          "BMC",
			inventoryDescription: "Vendor BMC Firmware",
			inventoryVersion:     "3.1.0",
			expectedVersion:      "3.1.0",
		},
		{
			name:                 "legacy BMC image description remains a fallback",
			inventoryDescription: "BMC image",
			inventoryVersion:     "3.2.0",
			expectedVersion:      "3.2.0",
		},
		{
			name:            "empty BMC inventory preserves the stored version",
			expectedVersion: "1.0.0",
		},
		{
			name:            "missing report preserves the stored version",
			omitReport:      true,
			expectedVersion: "1.0.0",
		},
		{
			name:             "failed component result preserves the stored version",
			inventoryVersion: "3.0.0",
			inventoryStatus:  corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_UNAVAILABLE,
			expectedVersion:  "1.0.0",
		},
		{
			name:            "inventory RPC failure preserves the stored version",
			inventoryErr:    errors.New("inventory unavailable"),
			expectedVersion: "1.0.0",
		},
		{
			name:                 "firmware update preserves a concurrent leak status update",
			canonicalVersion:     "4.0.0",
			concurrentLeakUpdate: true,
			expectedVersion:      "4.0.0",
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			dbConf, err := cdb.ConfigFromEnv()
			require.NoError(t, err)
			pool, err := utils.UnitTestDB(ctx, t, dbConf)
			require.NoError(t, err)

			const machineID = "fw-id"
			const bmcMAC = "aa:bb:cc:dd:ff:01"
			mockClient := nicoapi.NewMockClient()
			mockClient.AddMachine(nicoapi.MachineDetail{
				MachineID:       machineID,
				BmcMac:          bmcMAC,
				FirmwareVersion: "2.0.0",
				MachineType:     corev1.MachineType_HOST.String(),
			})
			mockClient.AddPowerState(machineID, nicoapi.PowerStateOn)

			componentID := machineID
			inventoryID := tc.inventoryID
			description := tc.inventoryDescription
			version := tc.inventoryVersion
			report := &corev1.EndpointExplorationReport{
				FirmwareVersions: map[string]string{"bmc": tc.canonicalVersion},
				Systems:          []*corev1.ComputerSystem{{PowerState: corev1.ComputerSystemPowerState_Off}},
				Service: []*corev1.Service{{
					Inventories: []*corev1.Inventory{{
						Id:          inventoryID,
						Description: &description,
						Version:     &version,
					}},
				}},
			}
			if tc.omitReport {
				report = nil
			}
			response := &corev1.GetComponentInventoryResponse{
				Entries: []*corev1.ComponentInventoryEntry{{
					Result: &corev1.ComponentResult{
						ComponentId: &componentID,
						Status:      tc.inventoryStatus,
					},
					Report: report,
				}},
			}
			client := &firmwareInventoryClient{
				Client:   mockClient,
				response: response,
				err:      tc.inventoryErr,
			}

			rack := model.Rack{
				Name:         "test-rack-fw",
				Manufacturer: "TestMfg",
				SerialNumber: "rack-serial-fw",
			}
			require.NoError(t, rack.Create(ctx, pool.DB))

			component := model.Component{
				SerialNumber:    "fw-serial",
				Manufacturer:    "TestMfg",
				RackID:          rack.ID,
				FirmwareVersion: "1.0.0",
				LeakStatus:      types.LeakStatusNotDetected,
			}
			require.NoError(t, component.Create(ctx, pool.DB))
			createTestBMC(ctx, t, pool, component.ID, bmcMAC)
			if tc.concurrentLeakUpdate {
				client.before = func() {
					_, updateErr := pool.DB.NewUpdate().
						Model(&model.Component{}).
						Set("leak_status = ?", types.LeakStatusDetected).
						Where("id = ?", component.ID).
						Exec(ctx)
					require.NoError(t, updateErr)
				}
			}

			// expectedSyncEnabled=false: actual-sync only. See TestInventory for
			// why the mirror must stay off when the mock has no expected machines.
			runInventoryOne(ctx, pool, client, false)

			var updated model.Component
			err = pool.DB.NewSelect().Model(&updated).Where("id = ?", component.ID).Scan(ctx)
			require.NoError(t, err)
			assert.Equal(t, tc.expectedVersion, updated.FirmwareVersion)
			require.NotNil(t, updated.PowerState)
			assert.Equal(t, nicoapi.PowerStateOn, *updated.PowerState)
			if tc.concurrentLeakUpdate {
				assert.Equal(t, types.LeakStatusDetected, updated.LeakStatus)
			}

			require.NotNil(t, client.request)
			requestedIDs := client.request.GetMachineIds().GetMachineIds()
			require.Len(t, requestedIDs, 1)
			assert.Equal(t, machineID, requestedIDs[0].GetId())
		})
	}
}

func TestBMCFirmwareVersionExactIDWinsOverEarlierDescription(t *testing.T) {
	description := "BMC image"
	descriptionVersion := "description-version"
	exactIDVersion := "exact-id-version"
	report := &corev1.EndpointExplorationReport{
		Service: []*corev1.Service{{
			Inventories: []*corev1.Inventory{
				{Id: "other", Description: &description, Version: &descriptionVersion},
				{Id: "BMC", Version: &exactIDVersion},
			},
		}},
	}

	assert.Equal(t, exactIDVersion, bmcFirmwareVersion(report))
}

func TestBMCFirmwareVersionHostIDWinsOverAcceleratorBMCs(t *testing.T) {
	description := "BMC image"
	hostVersion := "25.06-2_NV_WW_02"
	hgxVersion := "GB200Nvl-25.06-A"
	mgxVersion := "MGX-25.06-A"
	host := &corev1.Inventory{Id: "FW_BMC_0", Description: &description, Version: &hostVersion}
	hgx := &corev1.Inventory{Id: "HGX_FW_BMC_0", Description: &description, Version: &hgxVersion}
	mgx := &corev1.Inventory{Id: "MGX_FW_BMC_0", Description: &description, Version: &mgxVersion}

	testCases := []struct {
		name        string
		inventories []*corev1.Inventory
	}{
		{
			name:        "host BMC follows accelerator BMCs",
			inventories: []*corev1.Inventory{hgx, mgx, host},
		},
		{
			name:        "host BMC precedes accelerator BMCs",
			inventories: []*corev1.Inventory{host, hgx, mgx},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			report := &corev1.EndpointExplorationReport{
				Service: []*corev1.Service{{Inventories: tc.inventories}},
			}

			assert.Equal(t, hostVersion, bmcFirmwareVersion(report))
		})
	}
}

// TestApplyInventoryToComponentsPreservesConcurrentFields verifies that the
// shared switch and power-shelf projection updates only its owned columns.
func TestApplyInventoryToComponentsPreservesConcurrentFields(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		log.Warn().Msgf("Not running unit test due to no DB environment specified")
		t.SkipNow()
	}

	ctx := context.Background()
	dbConf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	require.NoError(t, err)

	rack := model.Rack{
		Name:         "test-rack-shared-inventory",
		Manufacturer: "TestMfg",
		SerialNumber: "rack-serial-shared-inventory",
	}
	require.NoError(t, rack.Create(ctx, pool.DB))

	componentID := "shared-inventory-id"
	component := model.Component{
		SerialNumber:    "shared-inventory-serial",
		Manufacturer:    "TestMfg",
		RackID:          rack.ID,
		ComponentID:     &componentID,
		FirmwareVersion: "1.0.0",
		LeakStatus:      types.LeakStatusNotDetected,
	}
	require.NoError(t, component.Create(ctx, pool.DB))

	var stale model.Component
	require.NoError(t, pool.DB.NewSelect().Model(&stale).Where("id = ?", component.ID).Scan(ctx))
	_, err = pool.DB.NewUpdate().
		Model(&model.Component{}).
		Set("leak_status = ?", types.LeakStatusDetected).
		Where("id = ?", component.ID).
		Exec(ctx)
	require.NoError(t, err)

	firmwareVersion := "2.0.0"
	responseID := componentID
	response := &corev1.GetComponentInventoryResponse{
		Entries: []*corev1.ComponentInventoryEntry{{
			Result: &corev1.ComponentResult{
				ComponentId: &responseID,
				Status:      corev1.ComponentManagerStatusCode_COMPONENT_MANAGER_STATUS_CODE_SUCCESS,
			},
			Report: &corev1.EndpointExplorationReport{
				FirmwareVersions: map[string]string{"bmc": firmwareVersion},
				Systems:          []*corev1.ComputerSystem{{PowerState: corev1.ComputerSystemPowerState_On}},
			},
		}},
	}

	applyInventoryToComponents(ctx, pool, response, map[string]*model.Component{componentID: &stale})

	var updated model.Component
	require.NoError(t, pool.DB.NewSelect().Model(&updated).Where("id = ?", component.ID).Scan(ctx))
	assert.Equal(t, firmwareVersion, updated.FirmwareVersion)
	require.NotNil(t, updated.PowerState)
	assert.Equal(t, nicoapi.PowerStateOn, *updated.PowerState)
	assert.Equal(t, types.LeakStatusDetected, updated.LeakStatus)
}

// TestSyncFirmwareSkipsUnmatchedMachineID verifies that a stale external ID
// absent from the current GetMachines snapshot is not sent to component
// inventory and cannot update a component marked missing in actual inventory.
func TestSyncFirmwareSkipsUnmatchedMachineID(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		log.Warn().Msgf("Not running unit test due to no DB environment specified")
		t.SkipNow()
	}

	ctx := context.Background()
	dbConf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	require.NoError(t, err)

	mockClient := nicoapi.NewMockClient()
	client := &firmwareInventoryClient{
		Client: mockClient,
		response: &corev1.GetComponentInventoryResponse{
			Entries: []*corev1.ComponentInventoryEntry{},
		},
	}

	rack := model.Rack{
		Name:         "test-rack-stale-fw",
		Manufacturer: "TestMfg",
		SerialNumber: "rack-serial-stale-fw",
	}
	require.NoError(t, rack.Create(ctx, pool.DB))

	staleMachineID := "stale-machine-id"
	component := model.Component{
		SerialNumber:    "stale-fw-serial",
		Manufacturer:    "TestMfg",
		RackID:          rack.ID,
		ComponentID:     &staleMachineID,
		FirmwareVersion: "1.0.0",
	}
	require.NoError(t, component.Create(ctx, pool.DB))
	createTestBMC(ctx, t, pool, component.ID, "aa:bb:cc:dd:ff:02")

	runInventoryOne(ctx, pool, client, false)

	assert.Nil(t, client.request, "component inventory must not be called for unmatched machine IDs")
	var updated model.Component
	require.NoError(t, pool.DB.NewSelect().Model(&updated).Where("id = ?", component.ID).Scan(ctx))
	assert.Equal(t, "1.0.0", updated.FirmwareVersion)
}

// TestSyncMachineIDs_DpuBmcNotLinked verifies that a compute component owning
// both a host BMC and a DPU BMC links to the HOST machine ID, never the DPU's.
// Core exposes the DPU as its own `MachineDetail`, so matching the unfiltered
// response could resolve the component to the DPU machine.
func TestSyncMachineIDs_DpuBmcNotLinked(t *testing.T) {
	ctx := context.Background()

	if os.Getenv("DB_PORT") == "" {
		log.Warn().Msgf("Not running unit test due to no DB environment specified")
		t.SkipNow()
	}

	dbConf, err := cdb.ConfigFromEnv()
	assert.Nil(t, err)
	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	assert.Nil(t, err)

	hostMac := "aa:bb:cc:dd:0a:01"
	dpuMac := "aa:bb:cc:dd:0a:02"

	// The DPU machine is listed first so that, without the host-only filter,
	// map-build order alone would not save us; the guard is the type filter.
	allDetails := []nicoapi.MachineDetail{
		{MachineID: "dpu-machine", BmcMac: dpuMac, MachineType: corev1.MachineType_DPU.String()},
		{MachineID: "host-machine", BmcMac: hostMac, MachineType: corev1.MachineType_HOST.String()},
	}

	rack := model.Rack{
		Name:         "test-rack-dpu",
		Manufacturer: "TestMfg",
		SerialNumber: "rack-serial-dpu",
	}
	err = rack.Create(ctx, pool.DB)
	assert.Nil(t, err)

	comp := model.Component{SerialNumber: "dpu-host-serial", Manufacturer: "TestMfg", RackID: rack.ID}
	err = comp.Create(ctx, pool.DB)
	assert.Nil(t, err)
	// Two BMCs on the same compute component: host and DPU.
	createTestBMC(ctx, t, pool, comp.ID, hostMac)
	dpuBMC := model.BMC{MacAddress: dpuMac, ComponentID: comp.ID, Type: "DPU"}
	_, err = pool.DB.NewInsert().Model(&dpuBMC).Exec(ctx)
	assert.Nil(t, err)

	components, err := model.GetComponentsByType(ctx, pool.DB, devicetypes.ComponentTypeCompute)
	assert.Nil(t, err)

	syncMachineIDs(ctx, pool, filterHostMachineDetails(allDetails), components)

	var updated model.Component
	err = pool.DB.NewSelect().Model(&updated).Where("id = ?", comp.ID).Scan(ctx)
	assert.Nil(t, err)
	assert.NotNil(t, updated.ComponentID)
	assert.Equal(t, "host-machine", *updated.ComponentID)
}
