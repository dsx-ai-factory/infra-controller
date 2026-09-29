// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package readiness

import (
	"context"
	"fmt"
	"sort"
	"strings"

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
