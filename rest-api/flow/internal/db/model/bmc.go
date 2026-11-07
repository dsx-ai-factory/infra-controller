// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"errors"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

type BMC struct {
	bun.BaseModel `bun:"table:bmc,alias:b"`

	MacAddress  string     `bun:"mac_address,pk"`
	Type        string     `bun:"type,type:varchar(16),default:'Unknown'"`
	ComponentID uuid.UUID  `bun:"component_id,type:uuid,notnull"`
	IPAddress   *string    `bun:"ip_address"`
	User        *string    `bun:"user"`
	Password    *string    `bun:"password"`
	Component   *Component `bun:"rel:belongs-to,join:component_id=id"`
}

func (bd *BMC) Create(ctx context.Context, tx bun.Tx) error {
	_, err := tx.NewInsert().Model(bd).Exec(ctx)
	return err
}

func (bd *BMC) Patch(ctx context.Context, idb bun.IDB) error {
	_, err := idb.NewUpdate().Model(bd).Where("mac_address = ?", bd.MacAddress).Exec(ctx)
	return err
}

// BuildPatch builds a patched BMC from the current BMC and the input
// BMC. It goes through the patchable fields and builds the patched BMC. If
// there is no change on patchable fields, it returns nil.
func (bd *BMC) BuildPatch(cur *BMC) *BMC {
	if bd == nil || cur == nil {
		// nothing to patch if either is nil
		return nil
	}

	// Make a copy fo the current BMC which serves as the base for the
	// patched BMC.
	patchedBMC := *cur
	patched := false

	// Go through the patchable fields which include:
	// IP address
	// Credential

	helper := func(np *string, cpp **string) {
		if cpp == nil {
			// Do not expect this, but handle it anyway.
			return
		}

		if np != nil && (*cpp == nil || *np != **cpp) {
			// Patch the current value with the input value
			ns := *np
			*cpp = &ns
			patched = true
		}
	}

	helper(bd.IPAddress, &patchedBMC.IPAddress)
	helper(bd.User, &patchedBMC.User)
	helper(bd.Password, &patchedBMC.Password)

	if !patched {
		return nil
	}

	return &patchedBMC
}

// InvalidType returns true if the BMC type is unknown.
func (bd *BMC) InvalidType() bool {
	return !devicetypes.IsValidBMCTypeString(bd.Type)
}

// ErrAmbiguousBMCMAC indicates that case variants of a BMC MAC belong to different components.
var ErrAmbiguousBMCMAC = errors.New("BMC MAC address matches multiple components")

// GetComponentByBMCMAC retrieves a unique component by its case-insensitive BMC MAC address.
// Returns the component with all its associated BMCs (needed for powershelf manager queries).
func GetComponentByBMCMAC(
	ctx context.Context,
	idb bun.IDB,
	macAddress string,
) (*Component, error) {
	// Select components rather than BMC rows so duplicate spellings owned by
	// the same component do not make the component identity ambiguous.
	matchingBMCs := idb.NewSelect().Model((*BMC)(nil)).Column("component_id").
		Where("lower(b.mac_address) = lower(?)", macAddress)
	var components []Component
	err := idb.NewSelect().
		Model(&components).
		Where("c.id IN (?)", matchingBMCs).
		Relation("BMCs").
		Relation("Rack").
		Limit(2).
		Scan(ctx)
	if err != nil {
		return nil, err
	}

	switch len(components) {
	case 0:
		return nil, sql.ErrNoRows
	case 1:
		return &components[0], nil
	default:
		return nil, ErrAmbiguousBMCMAC
	}
}
