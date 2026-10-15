// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package readiness

import (
	"context"
	"fmt"
	"sort"
	"strings"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// DBReader is the production StatusReader. It reads the component table
// via the supplied bun.IDB and exposes only the columns the gate needs.
type DBReader struct {
	idb bun.IDB
}

// NewDBReader builds a StatusReader backed by the given bun.IDB.
func NewDBReader(idb bun.IDB) *DBReader {
	return &DBReader{idb: idb}
}

// GetStatusesByExternalIDs implements StatusReader. The map key is the
// external_id string as supplied by the caller — components without a
// matching row (or with a NULL status) are simply absent.
func (r *DBReader) GetStatusesByExternalIDs(ctx context.Context, externalIDs []string) (map[string]*types.ComponentOperationStatus, error) {
	if len(externalIDs) == 0 {
		return map[string]*types.ComponentOperationStatus{}, nil
	}

	type row struct {
		bun.BaseModel `bun:"table:component,alias:c"`
		ExternalID    string                          `bun:"external_id"`
		Status        *types.ComponentOperationStatus `bun:"status"`
	}

	var rows []row
	err := r.idb.NewSelect().
		Model((*row)(nil)).
		Column("external_id", "status").
		Where("external_id IN (?)", bun.In(externalIDs)).
		Scan(ctx, &rows)
	if err != nil {
		return nil, fmt.Errorf("select component statuses: %w", err)
	}

	out := make(map[string]*types.ComponentOperationStatus, len(rows))
	for _, r := range rows {
		out[r.ExternalID] = r.Status
	}
	return out, nil
}

// GetHostExternalIDsByRackIDs implements StatusReader.
//
// Rack IDs are Core's external rack identifiers. They are resolved through
// rack.external_id because component.rack_id contains Flow's rack UUID, which
// is independent of the identifier Core assigns to the rack.
func (r *DBReader) GetHostExternalIDsByRackIDs(ctx context.Context, rackIDs []string) (map[string][]string, error) {
	if len(rackIDs) == 0 {
		return map[string][]string{}, nil
	}

	type row struct {
		bun.BaseModel  `bun:"table:rack,alias:r"`
		RackExternalID string  `bun:"rack_external_id"`
		HostExternalID *string `bun:"host_external_id"`
	}

	var rows []row
	err := r.idb.NewSelect().
		Model((*row)(nil)).
		ColumnExpr("r.external_id AS rack_external_id").
		ColumnExpr("c.external_id AS host_external_id").
		Join("LEFT JOIN component AS c").
		JoinOn("c.rack_id = r.id").
		JoinOn("c.type = ?", devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute)).
		JoinOn("c.external_id IS NOT NULL AND c.external_id != ''").
		JoinOn("c.deleted_at IS NULL").
		Where("r.external_id IN (?)", bun.In(rackIDs)).
		Where("r.deleted_at IS NULL").
		Scan(ctx, &rows)
	if err != nil {
		return nil, fmt.Errorf("select host components by rack: %w", err)
	}

	out := make(map[string][]string, len(rackIDs))
	for _, row := range rows {
		if _, ok := out[row.RackExternalID]; !ok {
			out[row.RackExternalID] = nil
		}
		if row.HostExternalID != nil {
			out[row.RackExternalID] = append(out[row.RackExternalID], *row.HostExternalID)
		}
	}

	var unresolved []string
	for _, rackID := range rackIDs {
		if _, ok := out[rackID]; !ok {
			unresolved = append(unresolved, rackID)
		}
	}
	if len(unresolved) > 0 {
		sort.Strings(unresolved)
		return nil, fmt.Errorf("resolve rack external IDs: no rack found for %s", strings.Join(unresolved, ", "))
	}

	return out, nil
}

var _ StatusReader = (*DBReader)(nil)

// GetStatusesByManagementMACs resolves active management BMC ownership and
// checks Flow inventory directly. A missing or ambiguous target is an error;
// a known component with no status retains the gate's permissive semantics.
func (r *DBReader) GetStatusesByManagementMACs(ctx context.Context, componentType devicetypes.ComponentType, macs []string) (map[string][]*types.ComponentOperationStatus, error) {
	out := make(map[string][]*types.ComponentOperationStatus, len(macs))
	if len(macs) == 0 {
		return out, nil
	}
	normalized := make([]string, len(macs))
	for i, mac := range macs {
		normalized[i] = strings.ToLower(mac)
	}
	type row struct {
		MAC          string
		Owner        uuid.UUID
		Type         string
		Status       *types.ComponentOperationStatus
		HostStatus   *types.ComponentOperationStatus
		RackID       *uuid.UUID
		ActiveRackID *uuid.UUID
	}
	var rows []row
	query := r.idb.NewSelect().TableExpr("bmc AS b").
		ColumnExpr("lower(b.mac_address) AS mac, c.id AS owner, c.type, c.status, h.status AS host_status, c.rack_id, r.id AS active_rack_id").
		Join("JOIN component AS c ON c.id = b.component_id AND c.deleted_at IS NULL").
		Join("LEFT JOIN rack AS r ON r.id = c.rack_id AND r.deleted_at IS NULL").
		Join("LEFT JOIN component AS h ON c.type != 'Compute' AND h.rack_id = r.id AND h.type = ? AND h.deleted_at IS NULL", devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute)).
		Where("b.type = ?", devicetypes.BMCTypeToString(devicetypes.BMCTypeHost)).
		Where("lower(b.mac_address) IN (?)", bun.In(normalized))
	err := query.Scan(ctx, &rows)
	if err != nil {
		return nil, fmt.Errorf("read management MAC readiness: %w", err)
	}
	owners := make(map[string]uuid.UUID)
	byMAC := make(map[string][]*types.ComponentOperationStatus)
	for _, row := range rows {
		if owner, ok := owners[row.MAC]; ok && owner != row.Owner {
			return nil, fmt.Errorf("ambiguous management MAC %s", row.MAC)
		}
		if row.Type != devicetypes.ComponentTypeToString(componentType) {
			return nil, fmt.Errorf("management MAC %s has unexpected component type %s", row.MAC, row.Type)
		}
		owners[row.MAC] = row.Owner
		if componentType != devicetypes.ComponentTypeCompute && row.RackID != nil && row.ActiveRackID == nil {
			return nil, fmt.Errorf("management MAC %s references a missing rack", row.MAC)
		}
		status := row.Status
		if componentType != devicetypes.ComponentTypeCompute {
			status = row.HostStatus
		}
		byMAC[row.MAC] = append(byMAC[row.MAC], status)
	}
	for _, mac := range macs {
		statuses, ok := byMAC[strings.ToLower(mac)]
		if !ok {
			return nil, fmt.Errorf("management MAC %s not found in Flow inventory", mac)
		}
		out[mac] = statuses
	}
	return out, nil
}
