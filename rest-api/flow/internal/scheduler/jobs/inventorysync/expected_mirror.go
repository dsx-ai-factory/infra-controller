// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// mirrorResult summarises one mirror pass for a single resource type. Used for
// structured logging so the operator can tell at a glance whether a sync was
// well-behaved (mostly updates, no surprises) or alarming (large delete
// counts).
type mirrorResult struct {
	resource         string
	pulled           int
	inserted         int
	updated          int
	adopted          int
	resurrected      int
	softDeleted      int
	skippedNoIDOrKey int
}

func (r mirrorResult) log() {
	log.Info().
		Str("resource", r.resource).
		Int("pulled", r.pulled).
		Int("inserted", r.inserted).
		Int("updated", r.updated).
		Int("adopted", r.adopted).
		Int("resurrected", r.resurrected).
		Int("soft_deleted", r.softDeleted).
		Int("skipped_invalid", r.skippedNoIDOrKey).
		Msgf("Expected-inventory mirror: %s", r.resource)
}

// syncExpectedFromCore pulls Core's expected inventory and reconciles each
// of Flow's tables to mirror it. Racks are reconciled first so a per-cycle
// rack_id → Rack.UUID map is available to resolve every component's
// RackExternalID into the FK Flow needs. Each resource type is independent:
// an RPC failure on machines doesn't stop switches from being reconciled.
//
// Runs immediately before runInventoryOne's drift detection so the drift
// loop sees a Flow inventory that's already aligned with Core's expected
// view.
func syncExpectedFromCore(
	ctx context.Context,
	pool *cdb.Session,
	nicoClient nicoapi.Client,
) {
	racks, rackOK := pullExpectedRacks(ctx, nicoClient)
	if rackOK {
		result := mirrorExpectedRacks(ctx, pool, racks)
		result.log()
	}

	// Build the cross-reference map after rack mirror so component specs
	// referencing rack_id strings can resolve them to Flow Rack.UUIDs. Done
	// via a fresh DB read instead of returning the map from
	// mirrorExpectedRacks so the rack mirror's signature stays focused; the
	// read is cheap (rack count is small) and includes adoptions / inserts
	// the mirror just made.
	rackIDByExtID, err := loadRackIDByExternalID(ctx, pool.DB)
	if err != nil {
		log.Error().Err(err).Msg("Expected-inventory mirror: loading rack external_id map failed; skipping component mirror this cycle")
		return
	}

	if machines, ok := pullExpectedMachines(ctx, nicoClient); ok {
		specs := make([]expectedComponentSpec, 0, len(machines))
		for _, m := range machines {
			specs = append(specs, machineDetailToSpec(m))
		}
		result := mirrorExpectedComponents(ctx, pool,
			devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
			specs, rackIDByExtID)
		result.log()
	}

	if switches, ok := pullExpectedSwitches(ctx, nicoClient); ok {
		specs := make([]expectedComponentSpec, 0, len(switches))
		for _, s := range switches {
			specs = append(specs, switchDetailToSpec(s))
		}
		result := mirrorExpectedComponents(ctx, pool,
			devicetypes.ComponentTypeToString(devicetypes.ComponentTypeNVSwitch),
			specs, rackIDByExtID)
		result.log()
	}

	if shelves, ok := pullExpectedPowerShelves(ctx, nicoClient); ok {
		specs := make([]expectedComponentSpec, 0, len(shelves))
		for _, ps := range shelves {
			specs = append(specs, powerShelfDetailToSpec(ps))
		}
		result := mirrorExpectedComponents(ctx, pool,
			devicetypes.ComponentTypeToString(devicetypes.ComponentTypePowerShelf),
			specs, rackIDByExtID)
		result.log()
	}
}

// loadRackIDByExternalID returns a map keyed by rack.external_id (Core's
// rack_id string) of the matching Flow Rack.UUID. Soft-deleted rows are
// excluded because component specs that reference a deleted rack would
// inherit a stale FK; better to skip the component spec with a warn.
func loadRackIDByExternalID(ctx context.Context, idb bun.IDB) (map[string]uuid.UUID, error) {
	var rows []struct {
		ID         uuid.UUID `bun:"id"`
		ExternalID *string   `bun:"external_id"`
	}
	if err := idb.NewSelect().
		Model((*model.Rack)(nil)).
		Column("id", "external_id").
		Where("external_id IS NOT NULL AND external_id <> ''").
		Scan(ctx, &rows); err != nil {
		return nil, err
	}
	out := make(map[string]uuid.UUID, len(rows))
	for _, r := range rows {
		if r.ExternalID != nil && *r.ExternalID != "" {
			out[*r.ExternalID] = r.ID
		}
	}
	return out, nil
}

// naturalKey joins manufacturer and serial number with a NUL byte, which can't
// appear inside either half, so the result is collision-free without escaping.
// Both mirrors key their "is this row already known" maps off it, so it lives
// here next to the orchestrator rather than in either type-specific file.
func naturalKey(manufacturer, serialNumber string) string {
	return manufacturer + "\x00" + serialNumber
}

// naturalKeyOrEmpty returns the empty string unless both halves are populated.
// A half-populated pair identifies nothing: every row missing the same half
// shares one key, so a map built on it hands an arbitrary one of them back.
// Callers skip such rows and match them by their real identity instead.
func naturalKeyOrEmpty(manufacturer, serialNumber string) string {
	if manufacturer == "" || serialNumber == "" {
		return ""
	}
	return naturalKey(manufacturer, serialNumber)
}
