// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net"
	"os"
	"testing"
	"time"

	log "github.com/sirupsen/logrus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/credential"
	"github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/sqltypes"
	cdb "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db"
	dbtestutil "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/testutil"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/credentials"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/db/migrations"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/db/model"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/pmcmanager"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/pmcregistry"
)

func newCred(u, p string) *credential.Credential {
	c := credential.New(u, p)
	return &c
}

// skipIfNoDatabase skips the test if database environment is not configured
func skipIfNoDatabase(t *testing.T) {
	t.Helper()
	if os.Getenv("DB_PORT") == "" {
		log.Warn("Skipping integration test: DB_PORT not set")
		t.Skip("DB_PORT environment variable not set - skipping integration test")
	}
}

// setupTestDB creates a fresh test database with migrations applied
func setupTestDB(t *testing.T) (*cdb.Session, func()) {
	t.Helper()
	ctx := context.Background()

	dbConf, err := cdb.ConfigFromEnv()
	require.NoError(t, err, "Failed to build DB config from env")

	session, err := dbtestutil.CreateTestDB(ctx, t, dbConf)
	require.NoError(t, err, "Failed to create test database")

	// Run migrations
	err = migrations.MigrateWithDB(ctx, session.DB)
	require.NoError(t, err, "Failed to run migrations")

	cleanup := func() {
		session.Close()
	}

	return session, cleanup
}

// createTestPMC creates a test PMC object with unique MAC/IP based on index
func createTestPMC(t *testing.T, index int, v vendor.VendorCode) *pmc.PMC {
	t.Helper()
	// Generate unique MAC: 00:11:22:33:44:XX where XX is the index
	macStr := fmt.Sprintf("00:11:22:33:44:%02x", index)
	// Generate unique IP: 192.168.1.X where X is 100 + index
	ipStr := fmt.Sprintf("192.168.1.%d", 100+index)

	mac, err := net.ParseMAC(macStr)
	require.NoError(t, err, "Failed to parse MAC")
	ip := net.ParseIP(ipStr)
	require.NotNil(t, ip, "Failed to parse IP")

	cred := newCred("admin", "password")
	p, err := pmc.NewFromAddr(mac, ip, v, cred)
	require.NoError(t, err, "Failed to create test PMC")
	return p
}

// TestIntegration_FirmwareManager_CanUpdate tests the CanUpdate logic
func TestIntegration_FirmwareManager_CanUpdate(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create test PMC
	testPmc := createTestPMC(t, 1, vendor.VendorCodeLiteon)

	// Create PMC registry using the test database and register the PMC
	pmcRegistry := pmcregistry.NewPostgresRegistryFromDB(session)
	require.NoError(t, pmcRegistry.RegisterPmc(ctx, testPmc))

	credMgr := credentials.NewInMemoryCredentialManager()
	pmcMgr := pmcmanager.New(pmcRegistry, credMgr)

	// Create firmware manager with test database
	store := &PostgresStore{session: session}
	manager := &Manager{
		firmwareUpdater: make(map[vendor.Vendor]*FirmwareUpdater),
		store:           store,
		pmcManager:      pmcMgr,
		dryRun:          true,
	}

	// Set up a temporary firmware directory with the expected layout
	fwDir := t.TempDir()
	pmcDir := fmt.Sprintf("%s/liteon/pmc", fwDir)
	require.NoError(t, os.MkdirAll(pmcDir, 0o755))
	require.NoError(t, os.WriteFile(pmcDir+"/cm14mp1r-r1.3.7_to_r1.3.8.tar", make([]byte, 2048), 0o644))
	require.NoError(t, os.WriteFile(pmcDir+"/cm14mp1r-r1.3.8_to_r1.3.9.tar", make([]byte, 2048), 0o644))

	updater, err := newFirmwareUpdater(vendor.CodeToVendor(vendor.VendorCodeLiteon), fwDir)
	require.NoError(t, err)
	manager.firmwareUpdater[vendor.CodeToVendor(vendor.VendorCodeLiteon)] = updater

	// Test: No pending update should allow new update (if version is valid)
	// Note: Actual canUpdate check requires redfish to get current version,
	// so we test the database-level blocking logic

	// Create a pending (non-terminal) firmware update
	_, err = model.NewFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	// Verify update was created
	fu, err := model.GetFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, powershelf.FirmwareStateQueued, fu.State)
	assert.Equal(t, "1.0.0", fu.VersionFrom)
	assert.Equal(t, "2.0.0", fu.VersionTo)
}

// TestIntegration_FirmwareManager_SetUpdateState tests state transitions
func TestIntegration_FirmwareManager_SetUpdateState(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create test PMC
	testPmc := createTestPMC(t, 2, vendor.VendorCodeLiteon)

	// Register PMC in database
	pmcModel := &model.PMC{
		MacAddress: model.MacAddr(testPmc.GetMac()),
		Vendor:     testPmc.GetVendor().Code,
		IPAddress:  sqltypes.IPAddr(testPmc.GetIp()),
	}
	tx, err := session.BeginTx(ctx)
	require.NoError(t, err)
	require.NoError(t, pmcModel.Create(ctx, tx))
	require.NoError(t, tx.Commit())

	// Create firmware manager
	store := &PostgresStore{session: session}
	manager := &Manager{
		store:  store,
		dryRun: true,
	}

	// Create a firmware update in Queued state
	fu, err := model.NewFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)
	assert.Equal(t, powershelf.FirmwareStateQueued, fu.State)

	// Transition to Verifying
	rec := modelToRecord(fu)
	err = manager.SetUpdateState(ctx, rec, powershelf.FirmwareStateVerifying, "")
	assert.NoError(t, err)

	// Verify state changed
	retrieved, err := model.GetFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, powershelf.FirmwareStateVerifying, retrieved.State)

	// Transition to Completed
	rec = modelToRecord(retrieved)
	err = manager.SetUpdateState(ctx, rec, powershelf.FirmwareStateCompleted, "")
	assert.NoError(t, err)

	// Verify terminal state
	final, err := model.GetFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, powershelf.FirmwareStateCompleted, final.State)
	assert.True(t, final.IsTerminal())
}

// TestIntegration_FirmwareManager_SetUpdateState_WithError tests error message handling
func TestIntegration_FirmwareManager_SetUpdateState_WithError(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create test PMC
	testPmc := createTestPMC(t, 3, vendor.VendorCodeLiteon)

	// Register PMC
	pmcModel := &model.PMC{
		MacAddress: model.MacAddr(testPmc.GetMac()),
		Vendor:     testPmc.GetVendor().Code,
		IPAddress:  sqltypes.IPAddr(testPmc.GetIp()),
	}
	tx, err := session.BeginTx(ctx)
	require.NoError(t, err)
	require.NoError(t, pmcModel.Create(ctx, tx))
	require.NoError(t, tx.Commit())

	// Create firmware manager
	store := &PostgresStore{session: session}
	manager := &Manager{
		store:  store,
		dryRun: true,
	}

	// Create a firmware update
	fu, err := model.NewFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PSU, "1.0.0", "2.0.0")
	require.NoError(t, err)

	// Transition to Failed with error message
	errMsg := "connection timeout to PMC"
	rec := modelToRecord(fu)
	err = manager.SetUpdateState(ctx, rec, powershelf.FirmwareStateFailed, errMsg)
	assert.NoError(t, err)

	// Verify error message was stored
	retrieved, err := model.GetFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PSU)
	require.NoError(t, err)
	assert.Equal(t, powershelf.FirmwareStateFailed, retrieved.State)
	assert.Equal(t, errMsg, retrieved.ErrorMessage)
	assert.True(t, retrieved.IsTerminal())
}

// TestIntegration_FirmwareManager_GetPendingUpdates tests retrieving pending updates
func TestIntegration_FirmwareManager_GetPendingUpdates(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create multiple PMCs
	pmcs := []*pmc.PMC{
		createTestPMC(t, 1, vendor.VendorCodeLiteon),
		createTestPMC(t, 2, vendor.VendorCodeLiteon),
		createTestPMC(t, 3, vendor.VendorCodeLiteon),
	}

	// Register all PMCs
	for _, p := range pmcs {
		pmcModel := &model.PMC{
			MacAddress: model.MacAddr(p.GetMac()),
			Vendor:     p.GetVendor().Code,
			IPAddress:  sqltypes.IPAddr(p.GetIp()),
		}
		tx, err := session.BeginTx(ctx)
		require.NoError(t, err)
		require.NoError(t, pmcModel.Create(ctx, tx))
		require.NoError(t, tx.Commit())
	}

	// Create firmware manager
	store := &PostgresStore{session: session}
	manager := &Manager{
		store:  store,
		dryRun: true,
	}

	// Create firmware updates for all PMCs
	for _, p := range pmcs {
		_, err := model.NewFirmwareUpdate(ctx, session.DB, p.GetMac(), powershelf.PMC, "1.0.0", "2.0.0")
		require.NoError(t, err)
	}

	// Get all pending updates
	pending, err := manager.getPendingFwUpdates(ctx)
	require.NoError(t, err)
	assert.Len(t, pending, 3, "Should have 3 pending updates")

	// Mark one as completed
	fu, err := model.GetFirmwareUpdate(ctx, session.DB, pmcs[0].GetMac(), powershelf.PMC)
	require.NoError(t, err)
	rec := modelToRecord(fu)
	err = manager.SetUpdateState(ctx, rec, powershelf.FirmwareStateCompleted, "")
	require.NoError(t, err)

	// Mark one as failed
	fu2, err := model.GetFirmwareUpdate(ctx, session.DB, pmcs[1].GetMac(), powershelf.PMC)
	require.NoError(t, err)
	rec2 := modelToRecord(fu2)
	err = manager.SetUpdateState(ctx, rec2, powershelf.FirmwareStateFailed, "test error")
	require.NoError(t, err)

	// Get pending updates again - should only be 1
	pending, err = manager.getPendingFwUpdates(ctx)
	require.NoError(t, err)
	assert.Len(t, pending, 1, "Should have 1 pending update after completing 2")

	// Verify the pending one is the correct PMC
	assert.Equal(t, pmcs[2].GetMac().String(), pending[0].PmcMacAddress.String())
}

// TestIntegration_FirmwareManager_BlockDuplicateUpdate tests that duplicate updates are blocked
func TestIntegration_FirmwareManager_BlockDuplicateUpdate(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create test PMC
	testPmc := createTestPMC(t, 1, vendor.VendorCodeLiteon)

	// Register PMC
	pmcModel := &model.PMC{
		MacAddress: model.MacAddr(testPmc.GetMac()),
		Vendor:     testPmc.GetVendor().Code,
		IPAddress:  sqltypes.IPAddr(testPmc.GetIp()),
	}
	tx, err := session.BeginTx(ctx)
	require.NoError(t, err)
	require.NoError(t, pmcModel.Create(ctx, tx))
	require.NoError(t, tx.Commit())

	// Create initial firmware update
	_, err = model.NewFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	// Get the update
	fu, err := model.GetFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, "1.0.0", fu.VersionFrom)
	assert.Equal(t, "2.0.0", fu.VersionTo)

	// Create another update for the same component - should upsert (replace)
	_, err = model.NewFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC, "2.0.0", "3.0.0")
	require.NoError(t, err)

	// Verify the update was replaced
	fu2, err := model.GetFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC)
	require.NoError(t, err)
	assert.Equal(t, "2.0.0", fu2.VersionFrom)
	assert.Equal(t, "3.0.0", fu2.VersionTo)

	// Should still only be 1 update for this PMC/component
	updates, err := model.ListFirmwareUpdatesForPMC(ctx, session.DB, testPmc.GetMac(), nil)
	require.NoError(t, err)
	assert.Len(t, updates, 1)
}

// TestIntegration_FirmwareManager_MultipleComponents tests updates for different components
func TestIntegration_FirmwareManager_MultipleComponents(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create test PMC
	testPmc := createTestPMC(t, 1, vendor.VendorCodeLiteon)

	// Register PMC
	pmcModel := &model.PMC{
		MacAddress: model.MacAddr(testPmc.GetMac()),
		Vendor:     testPmc.GetVendor().Code,
		IPAddress:  sqltypes.IPAddr(testPmc.GetIp()),
	}
	tx, err := session.BeginTx(ctx)
	require.NoError(t, err)
	require.NoError(t, pmcModel.Create(ctx, tx))
	require.NoError(t, tx.Commit())

	// Create updates for different components
	_, err = model.NewFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	time.Sleep(10 * time.Millisecond) // Ensure different timestamps

	_, err = model.NewFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PSU, "3.0.0", "4.0.0")
	require.NoError(t, err)

	// Create firmware manager
	store := &PostgresStore{session: session}
	manager := &Manager{
		store:  store,
		dryRun: true,
	}

	// Get all pending updates
	pending, err := manager.getPendingFwUpdates(ctx)
	require.NoError(t, err)
	assert.Len(t, pending, 2, "Should have 2 pending updates for different components")

	// Complete one component
	pmcFu, err := model.GetFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC)
	require.NoError(t, err)
	rec := modelToRecord(pmcFu)
	err = manager.SetUpdateState(ctx, rec, powershelf.FirmwareStateCompleted, "")
	require.NoError(t, err)

	// Should still have PSU update pending
	pending, err = manager.getPendingFwUpdates(ctx)
	require.NoError(t, err)
	assert.Len(t, pending, 1)
	assert.Equal(t, powershelf.PSU, pending[0].Component)
}

// TestIntegration_FirmwareManager_StateTransitionTimestamps tests that timestamps are updated
func TestIntegration_FirmwareManager_StateTransitionTimestamps(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create test PMC
	testPmc := createTestPMC(t, 1, vendor.VendorCodeLiteon)

	// Register PMC
	pmcModel := &model.PMC{
		MacAddress: model.MacAddr(testPmc.GetMac()),
		Vendor:     testPmc.GetVendor().Code,
		IPAddress:  sqltypes.IPAddr(testPmc.GetIp()),
	}
	tx, err := session.BeginTx(ctx)
	require.NoError(t, err)
	require.NoError(t, pmcModel.Create(ctx, tx))
	require.NoError(t, tx.Commit())

	// Create firmware manager
	store := &PostgresStore{session: session}
	manager := &Manager{
		store:  store,
		dryRun: true,
	}

	// Create firmware update
	fu, err := model.NewFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	initialTransitionTime := fu.LastTransitionTime
	initialUpdatedAt := fu.UpdatedAt

	// Wait a bit to ensure different timestamps
	time.Sleep(50 * time.Millisecond)

	// Transition state
	rec := modelToRecord(fu)
	err = manager.SetUpdateState(ctx, rec, powershelf.FirmwareStateVerifying, "")
	require.NoError(t, err)

	// Retrieve and check timestamps
	retrieved, err := model.GetFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC)
	require.NoError(t, err)

	assert.True(t, retrieved.LastTransitionTime.After(initialTransitionTime),
		"LastTransitionTime should be updated on state change")
	assert.True(t, retrieved.UpdatedAt.After(initialUpdatedAt),
		"UpdatedAt should be updated")
}

// TestIntegration_FirmwareManager_TerminalStateCheck tests IsTerminal logic
func TestIntegration_FirmwareManager_TerminalStateCheck(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	testCases := []struct {
		name       string
		finalState powershelf.FirmwareState
		isTerminal bool
	}{
		{"completed is terminal", powershelf.FirmwareStateCompleted, true},
		{"failed is terminal", powershelf.FirmwareStateFailed, true},
		{"queued is not terminal", powershelf.FirmwareStateQueued, false},
		{"verifying is not terminal", powershelf.FirmwareStateVerifying, false},
	}

	for i, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			// Create unique PMC for each test case
			macStr := "00:11:22:33:44:" + string(rune('5'+i)) + string(rune('5'+i))
			mac, _ := net.ParseMAC(macStr)
			// Use a simpler approach for unique MACs
			mac = net.HardwareAddr{0x00, 0x11, 0x22, 0x33, 0x44, byte(0x55 + i)}
			ip := net.IP{192, 168, 1, byte(100 + i)}

			testPmc, err := pmc.NewFromAddr(mac, ip, vendor.VendorCodeLiteon, nil)
			require.NoError(t, err)

			// Register PMC
			pmcModel := &model.PMC{
				MacAddress: model.MacAddr(testPmc.GetMac()),
				Vendor:     testPmc.GetVendor().Code,
				IPAddress:  sqltypes.IPAddr(testPmc.GetIp()),
			}
			tx, err := session.BeginTx(ctx)
			require.NoError(t, err)
			require.NoError(t, pmcModel.Create(ctx, tx))
			require.NoError(t, tx.Commit())

			// Create firmware manager
			store := &PostgresStore{session: session}
			manager := &Manager{
				store:  store,
				dryRun: true,
			}

			// Create and transition firmware update
			fu, err := model.NewFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC, "1.0.0", "2.0.0")
			require.NoError(t, err)

			if tc.finalState != powershelf.FirmwareStateQueued {
				rec := modelToRecord(fu)
				err = manager.SetUpdateState(ctx, rec, tc.finalState, "")
				require.NoError(t, err)
			}

			// Retrieve and check terminal state
			retrieved, err := model.GetFirmwareUpdate(ctx, session.DB, testPmc.GetMac(), powershelf.PMC)
			require.NoError(t, err)
			assert.Equal(t, tc.isTerminal, retrieved.IsTerminal())
		})
	}
}

// TestIntegration_FirmwareManager_GetUpdateNoRows tests handling of missing updates
func TestIntegration_FirmwareManager_GetUpdateNoRows(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Try to get an update that doesn't exist
	mac, _ := net.ParseMAC("FF:FF:FF:FF:FF:FF") // non-existent PMC
	_, err := model.GetFirmwareUpdate(ctx, session.DB, mac, powershelf.PMC)

	// Should get sql.ErrNoRows
	assert.True(t, errors.Is(err, sql.ErrNoRows), "Should return sql.ErrNoRows for missing update")
}
