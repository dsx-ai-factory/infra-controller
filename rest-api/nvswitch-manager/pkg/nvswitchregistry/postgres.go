// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvswitchregistry

import (
	"context"
	"database/sql"
	"fmt"
	"net"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/sqltypes"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvos"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
	log "github.com/sirupsen/logrus"
	"github.com/uptrace/bun"
)

// Ensure PostgresRegistry implements Registry.
var _ Registry = (*PostgresRegistry)(nil)

// PostgresRegistry is a PostgreSQL-backed implementation of Registry.
type PostgresRegistry struct {
	db *bun.DB
}

// NewPostgresRegistry creates a new PostgreSQL-backed registry.
func NewPostgresRegistry(db *bun.DB) *PostgresRegistry {
	return &PostgresRegistry{db: db}
}

// NVSwitchModel is the database model for nvswitch table.
type NVSwitchModel struct {
	bun.BaseModel `bun:"table:nvswitch,alias:ns"`

	UUID           uuid.UUID       `bun:"uuid,pk,type:uuid"`
	Vendor         int             `bun:"vendor,notnull"`
	BMCMACAddress  string          `bun:"bmc_mac_address,notnull"`
	BMCIPAddress   sqltypes.IPAddr `bun:"bmc_ip_address,notnull,type:inet"`
	BMCPort        int             `bun:"bmc_port,notnull,default:443"`
	NVOSMACAddress string          `bun:"nvos_mac_address,notnull"`
	NVOSIPAddress  sqltypes.IPAddr `bun:"nvos_ip_address,notnull,type:inet"`
	NVOSPort       int             `bun:"nvos_port,notnull,default:22"`
	RackID         string          `bun:"rack_id"`
}

// Start initializes the registry.
func (r *PostgresRegistry) Start(ctx context.Context) error {
	log.Info("PostgreSQL NVSwitch registry started")
	return nil
}

// Stop cleans up the registry.
func (r *PostgresRegistry) Stop(ctx context.Context) error {
	log.Info("PostgreSQL NVSwitch registry stopped")
	return nil
}

// Register creates or updates an NV-Switch tray.
// The select (lookup by BMC MAC) and the resulting insert or update run in a single
// transaction to avoid races when two concurrent calls register the same tray.
func (r *PostgresRegistry) Register(ctx context.Context, tray *nvswitch.NVSwitchTray) (uuid.UUID, bool, error) {
	if tray == nil {
		return uuid.Nil, false, fmt.Errorf("tray is nil")
	}
	if tray.BMC == nil {
		return uuid.Nil, false, fmt.Errorf("tray.BMC is nil")
	}
	if tray.NVOS == nil {
		return uuid.Nil, false, fmt.Errorf("tray.NVOS is nil")
	}

	bmcMAC := tray.BMC.MAC.String()

	var outID uuid.UUID
	var outCreated bool

	err := r.db.RunInTx(ctx, &sql.TxOptions{}, func(ctx context.Context, tx bun.Tx) error {
		// Check if this BMC MAC is already registered (within this transaction)
		existing := &NVSwitchModel{}
		err := tx.NewSelect().
			Model(existing).
			Where("bmc_mac_address = ?", bmcMAC).
			Scan(ctx)

		if err == nil {
			// Update existing entry
			existing.Vendor = int(tray.Vendor.Code)
			existing.BMCIPAddress = sqltypes.IPAddr(tray.BMC.IP)
			existing.BMCPort = tray.BMC.GetPort()
			existing.NVOSMACAddress = tray.NVOS.MAC.String()
			existing.NVOSIPAddress = sqltypes.IPAddr(tray.NVOS.IP)
			existing.NVOSPort = tray.NVOS.GetPort()
			existing.RackID = tray.RackID

			_, err = tx.NewUpdate().
				Model(existing).
				WherePK().
				Exec(ctx)
			if err != nil {
				return fmt.Errorf("failed to update nvswitch: %w", err)
			}

			outID = existing.UUID
			outCreated = false
			return nil
		}

		if err != sql.ErrNoRows {
			return fmt.Errorf("failed to check existing nvswitch: %w", err)
		}

		// Generate new UUID if not set
		if tray.UUID == uuid.Nil {
			tray.UUID = uuid.New()
		}

		model := &NVSwitchModel{
			UUID:           tray.UUID,
			Vendor:         int(tray.Vendor.Code),
			BMCMACAddress:  bmcMAC,
			BMCIPAddress:   sqltypes.IPAddr(tray.BMC.IP),
			BMCPort:        tray.BMC.GetPort(),
			NVOSMACAddress: tray.NVOS.MAC.String(),
			NVOSIPAddress:  sqltypes.IPAddr(tray.NVOS.IP),
			NVOSPort:       tray.NVOS.GetPort(),
			RackID:         tray.RackID,
		}

		_, err = tx.NewInsert().Model(model).Exec(ctx)
		if err != nil {
			return fmt.Errorf("failed to insert nvswitch: %w", err)
		}

		outID = tray.UUID
		outCreated = true
		return nil
	})

	if err != nil {
		return uuid.Nil, false, err
	}

	return outID, outCreated, nil
}

// Get retrieves an NV-Switch by UUID.
func (r *PostgresRegistry) Get(ctx context.Context, id uuid.UUID) (*nvswitch.NVSwitchTray, error) {
	model := &NVSwitchModel{}
	err := r.db.NewSelect().
		Model(model).
		Where("uuid = ?", id).
		Scan(ctx)

	if err != nil {
		if err == sql.ErrNoRows {
			return nil, fmt.Errorf("NV-Switch with UUID %s not found", id)
		}
		return nil, fmt.Errorf("failed to get nvswitch: %w", err)
	}

	return modelToTray(model)
}

// GetByBMCMAC retrieves an NV-Switch by BMC MAC address.
func (r *PostgresRegistry) GetByBMCMAC(ctx context.Context, bmcMAC string) (*nvswitch.NVSwitchTray, error) {
	model := &NVSwitchModel{}
	err := r.db.NewSelect().
		Model(model).
		Where("bmc_mac_address = ?", bmcMAC).
		Scan(ctx)

	if err != nil {
		if err == sql.ErrNoRows {
			return nil, fmt.Errorf("NV-Switch with BMC MAC %s not found", bmcMAC)
		}
		return nil, fmt.Errorf("failed to get nvswitch by BMC MAC: %w", err)
	}

	return modelToTray(model)
}

// List returns all registered NV-Switches.
func (r *PostgresRegistry) List(ctx context.Context) ([]*nvswitch.NVSwitchTray, error) {
	var models []*NVSwitchModel
	err := r.db.NewSelect().
		Model(&models).
		Order("uuid ASC").
		Scan(ctx)

	if err != nil {
		return nil, fmt.Errorf("failed to list nvswitches: %w", err)
	}

	result := make([]*nvswitch.NVSwitchTray, 0, len(models))
	for _, m := range models {
		tray, err := modelToTray(m)
		if err != nil {
			log.Warnf("Failed to convert model to tray: %v", err)
			continue
		}
		result = append(result, tray)
	}

	return result, nil
}

// Delete removes an NV-Switch by UUID.
func (r *PostgresRegistry) Delete(ctx context.Context, id uuid.UUID) error {
	result, err := r.db.NewDelete().
		Model((*NVSwitchModel)(nil)).
		Where("uuid = ?", id).
		Exec(ctx)

	if err != nil {
		return fmt.Errorf("failed to delete nvswitch: %w", err)
	}

	rowsAffected, _ := result.RowsAffected()
	if rowsAffected == 0 {
		return nil // Already deleted, treat as success
	}

	return nil
}

// modelToTray converts a database model to NVSwitchTray.
func modelToTray(m *NVSwitchModel) (*nvswitch.NVSwitchTray, error) {
	bmcMAC, err := net.ParseMAC(m.BMCMACAddress)
	if err != nil {
		return nil, fmt.Errorf("invalid BMC MAC address %s: %w", m.BMCMACAddress, err)
	}

	bmcIP := m.BMCIPAddress.IP()
	if bmcIP == nil {
		return nil, fmt.Errorf("invalid BMC IP address: %s", m.BMCIPAddress.String())
	}

	nvosMAC, err := net.ParseMAC(m.NVOSMACAddress)
	if err != nil {
		return nil, fmt.Errorf("invalid NVOS MAC address %s: %w", m.NVOSMACAddress, err)
	}

	nvosIP := m.NVOSIPAddress.IP()
	if nvosIP == nil {
		return nil, fmt.Errorf("invalid NVOS IP address: %s", m.NVOSIPAddress.String())
	}

	bmcObj, err := bmc.NewFromAddr(bmcMAC, bmcIP, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create BMC object: %w", err)
	}
	bmcObj.SetPort(m.BMCPort)

	nvosObj, err := nvos.NewFromAddr(nvosMAC, nvosIP, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create NVOS object: %w", err)
	}
	nvosObj.SetPort(m.NVOSPort)

	return &nvswitch.NVSwitchTray{
		UUID:   m.UUID,
		Vendor: vendor.CodeToVendor(vendor.VendorCode(m.Vendor)),
		RackID: m.RackID,
		BMC:    bmcObj,
		NVOS:   nvosObj,
	}, nil
}
