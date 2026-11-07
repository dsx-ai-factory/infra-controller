// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"net"
	"os"
	"testing"
	"time"

	log "github.com/sirupsen/logrus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/sqltypes"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	dbtestutil "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/testutil"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/db/migrations"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
)

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

// parseMac is a test helper that parses a MAC address and converts to MacAddr
func parseMac(t *testing.T, s string) MacAddr {
	t.Helper()
	mac, err := net.ParseMAC(s)
	require.NoError(t, err)
	return MacAddr(mac)
}

// parseIP is a test helper that parses an IP address and converts to IPAddr
func parseIP(t *testing.T, s string) sqltypes.IPAddr {
	t.Helper()
	ip := net.ParseIP(s)
	require.NotNil(t, ip)
	return sqltypes.IPAddr(ip)
}

func TestIntegration_PMC_CreateAndGet(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create PMC with native types
	mac := parseMac(t, "00:11:22:33:44:55")
	ip := parseIP(t, "192.168.1.100")

	pmc := &PMC{
		MacAddress: mac,
		Vendor:     vendor.VendorCodeLiteon,
		IPAddress:  ip,
	}

	// Insert via transaction
	tx, err := session.BeginTx(ctx)
	require.NoError(t, err)

	err = pmc.Create(ctx, tx)
	assert.NoError(t, err, "Create should succeed")

	err = tx.Commit()
	require.NoError(t, err)

	// Retrieve by MAC
	query := &PMC{MacAddress: mac}
	retrieved, err := query.Get(ctx, session.DB)
	assert.NoError(t, err, "Get by MAC should succeed")
	assert.NotNil(t, retrieved)
	assert.Equal(t, "00:11:22:33:44:55", retrieved.MacAddress.String())
	assert.Equal(t, "192.168.1.100", retrieved.IPAddress.String())
	assert.Equal(t, vendor.VendorCodeLiteon, retrieved.Vendor)

	// Retrieve by IP
	queryByIP := &PMC{IPAddress: ip}
	retrievedByIP, err := queryByIP.Get(ctx, session.DB)
	assert.NoError(t, err, "Get by IP should succeed")
	assert.NotNil(t, retrievedByIP)
	assert.Equal(t, "00:11:22:33:44:55", retrievedByIP.MacAddress.String())
}

func TestIntegration_PMC_Patch(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create PMC
	mac := parseMac(t, "AA:BB:CC:DD:EE:FF")
	ip := parseIP(t, "10.0.0.1")

	pmc := &PMC{
		MacAddress: mac,
		Vendor:     vendor.VendorCodeLiteon,
		IPAddress:  ip,
	}

	tx, err := session.BeginTx(ctx)
	require.NoError(t, err)
	require.NoError(t, pmc.Create(ctx, tx))
	require.NoError(t, tx.Commit())

	// Patch IP
	newIP := parseIP(t, "10.0.0.2")
	pmc.IPAddress = newIP
	err = pmc.Patch(ctx, session.DB)
	assert.NoError(t, err, "Patch should succeed")

	// Verify patch
	query := &PMC{MacAddress: mac}
	retrieved, err := query.Get(ctx, session.DB)
	assert.NoError(t, err)
	assert.Equal(t, "10.0.0.2", retrieved.IPAddress.String())
}

func TestIntegration_FirmwareUpdate_CreateAndGet(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// First create a PMC (foreign key reference)
	mac := parseMac(t, "00:11:22:33:44:55")
	ip := parseIP(t, "192.168.1.100")
	pmc := &PMC{
		MacAddress: mac,
		Vendor:     vendor.VendorCodeLiteon,
		IPAddress:  ip,
	}

	tx, err := session.BeginTx(ctx)
	require.NoError(t, err)
	require.NoError(t, pmc.Create(ctx, tx))
	require.NoError(t, tx.Commit())

	// Create FirmwareUpdate - use net.HardwareAddr for the function signature
	netMac := mac.HardwareAddr()
	fu, err := NewFirmwareUpdate(ctx, session.DB, netMac, powershelf.PMC, "1.0.0", "2.0.0")
	assert.NoError(t, err, "NewFirmwareUpdate should succeed")
	assert.NotNil(t, fu)
	assert.Equal(t, powershelf.FirmwareStateQueued, fu.State)

	// Get FirmwareUpdate
	retrieved, err := GetFirmwareUpdate(ctx, session.DB, netMac, powershelf.PMC)
	assert.NoError(t, err, "GetFirmwareUpdate should succeed")
	assert.NotNil(t, retrieved)
	assert.Equal(t, "1.0.0", retrieved.VersionFrom)
	assert.Equal(t, "2.0.0", retrieved.VersionTo)
	assert.Equal(t, powershelf.FirmwareStateQueued, retrieved.State)
}

func TestIntegration_FirmwareUpdate_UpdateState(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create PMC
	mac := parseMac(t, "AA:BB:CC:DD:EE:FF")
	ip := parseIP(t, "10.0.0.1")
	pmc := &PMC{
		MacAddress: mac,
		Vendor:     vendor.VendorCodeLiteon,
		IPAddress:  ip,
	}

	tx, err := session.BeginTx(ctx)
	require.NoError(t, err)
	require.NoError(t, pmc.Create(ctx, tx))
	require.NoError(t, tx.Commit())

	// Create FirmwareUpdate
	netMac := mac.HardwareAddr()
	fu, err := NewFirmwareUpdate(ctx, session.DB, netMac, powershelf.PSU, "1.0.0", "2.0.0")
	require.NoError(t, err)

	// Update state
	err = fu.UpdateFirmwareUpdateState(ctx, session.DB, powershelf.FirmwareStateCompleted, "")
	assert.NoError(t, err, "UpdateFirmwareUpdateState should succeed")

	// Verify
	retrieved, err := GetFirmwareUpdate(ctx, session.DB, netMac, powershelf.PSU)
	assert.NoError(t, err)
	assert.Equal(t, powershelf.FirmwareStateCompleted, retrieved.State)
	assert.True(t, retrieved.IsTerminal())
}

func TestIntegration_FirmwareUpdate_GetAllPending(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create multiple PMCs and firmware updates
	macStrs := []string{"00:11:22:33:44:55", "AA:BB:CC:DD:EE:FF", "11:22:33:44:55:66"}
	ips := []string{"192.168.1.1", "192.168.1.2", "192.168.1.3"}

	for i, macStr := range macStrs {
		mac := parseMac(t, macStr)
		ip := parseIP(t, ips[i])
		pmc := &PMC{MacAddress: mac, Vendor: vendor.VendorCodeLiteon, IPAddress: ip}

		tx, err := session.BeginTx(ctx)
		require.NoError(t, err)
		require.NoError(t, pmc.Create(ctx, tx))
		require.NoError(t, tx.Commit())

		_, err = NewFirmwareUpdate(ctx, session.DB, mac.HardwareAddr(), powershelf.PMC, "1.0.0", "2.0.0")
		require.NoError(t, err)
	}

	// Mark one as completed
	mac1 := parseMac(t, macStrs[0])
	fu, err := GetFirmwareUpdate(ctx, session.DB, mac1.HardwareAddr(), powershelf.PMC)
	require.NoError(t, err)
	require.NoError(t, fu.UpdateFirmwareUpdateState(ctx, session.DB, powershelf.FirmwareStateCompleted, ""))

	// Get all pending - should be 2
	pending, err := GetAllPendingFirmwareUpdates(ctx, session.DB)
	assert.NoError(t, err)
	assert.Len(t, pending, 2, "Should have 2 pending updates")
}

func TestIntegration_FirmwareUpdate_ListForPMC(t *testing.T) {
	skipIfNoDatabase(t)

	session, cleanup := setupTestDB(t)
	defer cleanup()

	ctx := context.Background()

	// Create PMC with multiple component updates
	mac := parseMac(t, "00:11:22:33:44:55")
	ip := parseIP(t, "192.168.1.100")
	pmc := &PMC{MacAddress: mac, Vendor: vendor.VendorCodeLiteon, IPAddress: ip}

	tx, err := session.BeginTx(ctx)
	require.NoError(t, err)
	require.NoError(t, pmc.Create(ctx, tx))
	require.NoError(t, tx.Commit())

	netMac := mac.HardwareAddr()

	// Create updates for different components
	_, err = NewFirmwareUpdate(ctx, session.DB, netMac, powershelf.PMC, "1.0.0", "2.0.0")
	require.NoError(t, err)

	// Wait a moment to ensure different created_at times
	time.Sleep(10 * time.Millisecond)

	_, err = NewFirmwareUpdate(ctx, session.DB, netMac, powershelf.PSU, "3.0.0", "4.0.0")
	require.NoError(t, err)

	// List all for PMC
	updates, err := ListFirmwareUpdatesForPMC(ctx, session.DB, netMac, nil)
	assert.NoError(t, err)
	assert.Len(t, updates, 2, "Should have 2 updates")

	// List filtered by component
	pmcComp := powershelf.PMC
	updates, err = ListFirmwareUpdatesForPMC(ctx, session.DB, netMac, &pmcComp)
	assert.NoError(t, err)
	assert.Len(t, updates, 1, "Should have 1 PMC update")
	assert.Equal(t, powershelf.PMC, updates[0].Component)
}
