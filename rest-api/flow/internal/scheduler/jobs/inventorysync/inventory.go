// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package inventorysync reconciles Flow's rack / component / BMC / drift
// tables against Core every cycle: syncExpectedFromCore mirrors Core's
// expected inventory, while runActualSync detects drift and projects observed
// topology.
//
// TODO: this job writes the DB directly via bun model.* and pool.RunInTx,
// bypassing the service -> inventorymanager -> store layering the rest of Flow
// uses, so the same tables now have two writers with different invariants
// (and the BMC reconciliation here duplicates store.PatchComponent). The
// store/manager API can't yet express what the mirror needs — per-type
// transactional batch reconcile, resurrection of soft-deleted rows,
// column-whitelist updates, tombstone GC, and a drift-table replace. Follow-up:
// add those as transactional batch methods on the store (e.g.
// ReconcileExpectedRacks / ReconcileExpectedComponents /
// ReplaceDriftsByComponentType) and
// route both halves of this job through the manager so there's a single
// writer. Tracked separately from the correctness fixes.
package inventorysync

import (
	"context"

	"github.com/rs/zerolog/log"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// runInventoryOne is a single iteration of the inventory sync job. Order:
//
//  1. syncExpectedFromCore mirrors Core's expected inventory into Flow's
//     rack / component tables (the "expected" half of the package — see
//     expected_mirror*.go). Gated by expectedSyncEnabled; when false the
//     step is skipped entirely and Flow's existing ingestion path is the
//     sole writer to rack / component.
//  2. runActualSync reconciles actual component state, projects valid NVLink
//     domain observations, and returns an independent result for Compute,
//     NVSwitch, and PowerShelf (the "actual" half — see actual_sync*.go).
//  3. Each successful type atomically replaces only its own drift rows. A
//     failed type preserves its previous rows, and one persistence failure does
//     not prevent later types from being attempted.
//
// Errors are handled inside each step: any per-type RPC failure is logged
// and that type's drifts are skipped, but the rest of the cycle continues.
// A persistence failure is also logged rather than propagated — the
// scheduler retries on the next trigger.
func runInventoryOne(
	ctx context.Context,
	pool *cdb.Session,
	nicoClient nicoapi.Client,
	expectedSyncEnabled bool,
) {
	if expectedSyncEnabled {
		syncExpectedFromCore(ctx, pool, nicoClient)
	} else {
		log.Debug().Msgf("Expected-inventory mirror: skipped this cycle (gate %s is off)", envExpectedSyncEnabled)
	}

	results := runActualSync(ctx, pool, nicoClient)
	allPersisted := true
	for _, result := range results {
		componentType := devicetypes.ComponentTypeToString(result.componentType)
		if !result.syncOK {
			allPersisted = false
			log.Warn().Str("component_type", componentType).
				Msg("Drift detection failed; preserving this component type's previous drift records")
			continue
		}

		if err := pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
			return model.ReplaceDriftsByComponentType(ctx, tx, componentType, result.drifts)
		}); err != nil {
			allPersisted = false
			log.Error().Err(err).Str("component_type", componentType).
				Msg("Unable to persist drift records")
			continue
		}
		log.Info().Str("component_type", componentType).
			Msg("Drift snapshot persisted")
	}

	if allPersisted {
		if err := pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
			return model.DeleteLegacyUnscopedDrifts(ctx, tx)
		}); err != nil {
			log.Error().Err(err).Msg("Unable to remove legacy unscoped drift records")
		}
	}
}
