// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"errors"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/sqltypes"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"

	"github.com/uptrace/bun"
)

// PMC is the database model for a power shelf PMC with unique MAC and IP and a vendor code.
type PMC struct {
	bun.BaseModel `bun:"table:pmc,alias:p"`

	MacAddress MacAddr           `bun:"mac_address,pk,unique,notnull,type:macaddr"`
	Vendor     vendor.VendorCode `bun:"vendor,notnull"`
	IPAddress  sqltypes.IPAddr   `bun:"ip_address,unique,notnull,type:inet"`
}

// Create inserts a new PMC row.
func (pmc *PMC) Create(ctx context.Context, tx bun.Tx) error {
	_, err := tx.NewInsert().Model(pmc).Exec(ctx)
	return err
}

// Patch updates the PMC row by MAC address.
func (pmc *PMC) Patch(ctx context.Context, idb bun.IDB) error {
	_, err := idb.NewUpdate().Model(pmc).Where("mac_address = ?", pmc.MacAddress).Exec(ctx)
	return err
}

// BuildPatch copies changed patchable fields from cur; returns nil if no changes.
func (pmc *PMC) BuildPatch(cur *PMC) *PMC {
	if pmc == nil || cur == nil {
		// nothing to patch if either is nil
		return nil
	}

	patched := false

	if !pmc.IPAddress.Equal(cur.IPAddress) {
		pmc.IPAddress = cur.IPAddress
		patched = true
	}

	if !patched {
		return nil
	}

	return pmc
}

// Get retrieves a PMC by MAC or IP (one must be specified).
func (pmc *PMC) Get(
	ctx context.Context,
	idb bun.IDB,
) (*PMC, error) {
	var retPmc PMC
	var query *bun.SelectQuery

	if pmc.MacAddress != nil {
		query = idb.NewSelect().Model(&retPmc).Where("mac_address = ?", pmc.MacAddress)
	} else if pmc.IPAddress != nil {
		query = idb.NewSelect().Model(&retPmc).Where("ip_address = ?", pmc.IPAddress)
	} else {
		return nil, errors.New("cannot query PMC without specifying either a MAC address or an IP address")
	}

	if err := query.Scan(ctx); err != nil {
		return nil, err
	}

	return &retPmc, nil
}

// InvalidType returns true if the PMC's vendor code is unsupported.
func (pmc *PMC) InvalidType() bool {
	return vendor.CodeToVendor(pmc.Vendor).IsSupported() != nil
}
