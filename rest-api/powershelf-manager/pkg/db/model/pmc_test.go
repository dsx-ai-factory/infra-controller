// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"net"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/sqltypes"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/stretchr/testify/assert"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"
)

// Helper to parse MAC address, panics on error (test helper)
func mustParseMAC(s string) MacAddr {
	if s == "" {
		return nil
	}
	mac, err := net.ParseMAC(s)
	if err != nil {
		panic(fmt.Sprintf("mustParseMAC(%q): %v", s, err))
	}
	return MacAddr(mac)
}

// Helper to parse IP address
func mustParseIP(s string) sqltypes.IPAddr {
	if s == "" {
		return nil
	}
	return sqltypes.IPAddr(net.ParseIP(s))
}

type testPMC struct {
	p *PMC
}

func newTestPMC(p PMC) *testPMC {
	return &testPMC{p: &p}
}

func (tpmc *testPMC) PMC() *PMC {
	return tpmc.p
}

func (tpmc *testPMC) modifyIP(ip string) *testPMC {
	tpmc.p.IPAddress = mustParseIP(ip)
	return tpmc
}

func (tpmc *testPMC) modifyVendor(v vendor.VendorCode) *testPMC {
	tpmc.p.Vendor = v
	return tpmc
}

func (tpmc *testPMC) modifyMac(mac string) *testPMC {
	tpmc.p.MacAddress = mustParseMAC(mac)
	return tpmc
}

func newMockIDB(t *testing.T) (*bun.DB, sqlmock.Sqlmock, func()) {
	t.Helper()
	sqlDB, mock, err := sqlmock.New()
	if err != nil {
		t.Fatalf("sqlmock.New error: %v", err)
	}
	bdb := bun.NewDB(sqlDB, pgdialect.New())
	cleanup := func() {
		_ = bdb.Close()
		_ = sqlDB.Close()
	}
	return bdb, mock, cleanup
}

func TestPMCBuildPatch(t *testing.T) {
	sharePMC := PMC{
		MacAddress: mustParseMAC("00:11:22:33:44:55"),
		Vendor:     vendor.VendorCodeLiteon,
		IPAddress:  mustParseIP("192.168.1.100"),
	}

	testCases := map[string]struct {
		cur      *PMC
		input    *PMC
		expected *PMC
	}{
		"nil input PMC returns nil": {
			cur:      newTestPMC(sharePMC).PMC(),
			input:    nil,
			expected: nil,
		},
		"nil current PMC returns nil": {
			cur:      nil,
			input:    newTestPMC(sharePMC).PMC(),
			expected: nil,
		},
		"both PMCs nil returns nil": {
			cur:      nil,
			input:    nil,
			expected: nil,
		},
		"no changes returns nil": {
			cur:      newTestPMC(sharePMC).PMC(),
			input:    newTestPMC(sharePMC).PMC(),
			expected: nil,
		},
		"IP address change (only IP is patchable)": {
			cur:      newTestPMC(sharePMC).modifyIP("192.168.1.101").PMC(),
			input:    newTestPMC(sharePMC).PMC(),
			expected: newTestPMC(sharePMC).modifyIP("192.168.1.101").PMC(),
		},
		"vendor change only (non-patchable) returns nil": {
			cur:      newTestPMC(sharePMC).PMC(),
			input:    newTestPMC(sharePMC).modifyVendor(vendor.VendorCodeUnsupported).PMC(),
			expected: nil,
		},
		"mac change only (non-patchable) returns nil": {
			cur:      newTestPMC(sharePMC).PMC(),
			input:    newTestPMC(sharePMC).modifyMac("AA:BB:CC:DD:EE:FF").PMC(),
			expected: nil,
		},
		"mixed changes: vendor, mac, and IP change (only IP patched)": {
			cur:      newTestPMC(sharePMC).modifyIP("192.168.1.200").PMC(),
			input:    newTestPMC(sharePMC).modifyVendor(vendor.VendorCodeUnsupported).modifyMac("AA:BB:CC:DD:EE:FF").PMC(),
			expected: newTestPMC(sharePMC).modifyVendor(vendor.VendorCodeUnsupported).modifyMac("AA:BB:CC:DD:EE:FF").modifyIP("192.168.1.200").PMC(),
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			result := tc.input.BuildPatch(tc.cur)
			assert.Equal(t, tc.expected, result)
		})
	}
}

func TestPMCInvalidType(t *testing.T) {
	testCases := map[string]struct {
		inVendor vendor.VendorCode
		expected bool
	}{
		"unsupported vendor": {
			inVendor: vendor.VendorCodeUnsupported,
			expected: true,
		},
		"supported vendor (Liteon)": {
			inVendor: vendor.VendorCodeLiteon,
			expected: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			p := newTestPMC(PMC{
				MacAddress: mustParseMAC("00:11:22:33:44:55"),
				Vendor:     tc.inVendor,
				IPAddress:  mustParseIP("192.168.1.100"),
			}).PMC()
			got := p.InvalidType()
			assert.Equal(t, tc.expected, got)
		})
	}
}

func TestPMCGet_NoIdentifier(t *testing.T) {
	testCases := map[string]struct {
		inPMC *PMC
	}{
		"no mac and no ip returns error": {
			inPMC: &PMC{},
		},
		"nil mac and nil ip returns error": {
			inPMC: &PMC{MacAddress: nil, IPAddress: nil},
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ret, err := tc.inPMC.Get(context.Background(), nil) // idb not used in this path
			assert.Error(t, err)
			assert.Nil(t, ret)
		})
	}
}

func TestPMCGet_DBError_ByMac(t *testing.T) {
	bdb, mock, cleanup := newMockIDB(t)
	defer cleanup()

	mock.ExpectQuery(`SELECT .* FROM "pmc" AS "p" WHERE \(mac_address = .*`).
		WillReturnError(fmt.Errorf("scan error"))

	in := &PMC{MacAddress: mustParseMAC("00:11:22:33:44:55")}
	ret, err := in.Get(context.Background(), bdb)
	assert.Error(t, err)
	assert.Nil(t, ret)
	assert.NoError(t, mock.ExpectationsWereMet())
}

func TestPMCCreate_Success(t *testing.T) {
	bdb, mock, cleanup := newMockIDB(t)
	defer cleanup()

	mock.ExpectBegin()
	mock.ExpectExec(`INSERT INTO "pmc"`).
		WillReturnResult(sqlmock.NewResult(1, 1))
	mock.ExpectCommit()

	tx, err := bdb.BeginTx(context.Background(), nil)
	assert.NoError(t, err)

	p := &PMC{
		MacAddress: mustParseMAC("00:11:22:33:44:55"),
		Vendor:     vendor.VendorCodeLiteon,
		IPAddress:  mustParseIP("192.168.1.100"),
	}
	err = p.Create(context.Background(), tx)
	assert.NoError(t, err)

	assert.NoError(t, tx.Commit())
	assert.NoError(t, mock.ExpectationsWereMet())
}

func TestPMCPatch_Success(t *testing.T) {
	bdb, mock, cleanup := newMockIDB(t)
	defer cleanup()

	mock.ExpectExec(`UPDATE "pmc"`).
		WillReturnResult(sqlmock.NewResult(1, 1))

	p := &PMC{
		MacAddress: mustParseMAC("00:11:22:33:44:55"),
		Vendor:     vendor.VendorCodeLiteon,
		IPAddress:  mustParseIP("192.168.1.101"),
	}
	err := p.Patch(context.Background(), bdb)
	assert.NoError(t, err)
	assert.NoError(t, mock.ExpectationsWereMet())
}
