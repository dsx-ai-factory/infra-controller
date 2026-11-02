// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"fmt"
	"reflect"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
)

// Well-known label keys Core writes on ExpectedRack.metadata.labels. Mirrored
// here so this package doesn't pull in the api-model crate's Rust constants.
// Keep in sync with crates/api-model/src/rack.rs.
const (
	labelChassisManufacturer = "chassis.manufacturer"
	labelChassisSerialNumber = "chassis.serial-number"
	labelChassisModel        = "chassis.model"
	labelLocationRegion      = "location.region"
	labelLocationDatacenter  = "location.datacenter"
	labelLocationRoom        = "location.room"
	labelLocationPosition    = "location.position"
)

// pullExpectedRacks wraps the nicoapi RPC with the single safety guard the
// mirror needs: an RPC failure returns rpcOK=false so the caller skips
// reconciliation entirely and leaves Flow untouched. A *successful* RPC is
// authoritative even when it returns zero rows — the caller then soft-deletes
// every mirror-adopted Flow rack, because Core saying "no racks" is a real
// state, not a blip. This relies on Core surfacing transient unavailability
// (restarts, mid-migration) as an RPC error rather than an empty result.
func pullExpectedRacks(
	ctx context.Context,
	nicoClient nicoapi.Client,
) (rows []nicoapi.ExpectedRackDetail, rpcOK bool) {
	rows, err := nicoClient.GetAllExpectedRackDetails(ctx)
	if err != nil {
		log.Error().Err(err).Msg("Expected-inventory mirror: pulling expected racks from Core failed; skipping rack mirror this cycle")
		return nil, false
	}
	if len(rows) == 0 {
		log.Warn().Msg("Expected-inventory mirror: Core returned zero expected racks; mirror will soft-delete all mirror-adopted Flow racks this cycle")
	}
	return rows, true
}

// mirrorExpectedRacks reconciles Flow's rack table against Core's
// expected_racks view. The algorithm is, in order:
//
//  1. Index every Flow rack — including soft-deleted ones — by external_id
//     (the mirrored rack's identity) and by (manufacturer, serial_number).
//     Only rows carrying both halves enter the second index; a half-populated
//     pair identifies nothing and would let unrelated racks match each other.
//     Including soft-deleted rows is what makes resurrection work: a rack that
//     briefly disappeared from Core and came back keeps its UUID, and a
//     re-insert would otherwise collide on the unique constraint the tombstone
//     still occupies.
//
//  2. For each Core row, find the matching Flow row by external_id, falling
//     back to (manufacturer, serial_number) to adopt rows the mirror has never
//     reached. New rows are inserted. A matched row that's currently
//     soft-deleted is resurrected by clearing deleted_at; mirror-managed
//     fields are updated alongside on real deltas. A Core rack carrying no
//     rack_id is skipped, there being no identity to store it under.
//
//  3. Live Flow rows whose external_id is set but no longer appear in Core
//     are soft-deleted (including the case where Core returned zero racks —
//     the caller only invokes this after a successful RPC, so empty is
//     authoritative). A legacy row with neither external_id nor a complete
//     (manufacturer, serial_number) identity is also soft-deleted because no
//     future snapshot can correlate it. A legacy row with a complete natural
//     key is adopted during step 2 when Core still reports that chassis;
//     otherwise it is stale and is deleted too. Soft-deleted rows Core doesn't
//     report are left alone (already gone).
//
// All writes for one pass happen in a single transaction so partial failures
// can't leave the table half-mirrored.
func mirrorExpectedRacks(
	ctx context.Context,
	pool *cdb.Session,
	coreRacks []nicoapi.ExpectedRackDetail,
) mirrorResult {
	result := mirrorResult{resource: "rack", pulled: len(coreRacks)}

	flowRacks, err := getAllRacksIncludingDeleted(ctx, pool.DB)
	if err != nil {
		log.Error().Err(err).Msg("Expected-inventory mirror: loading Flow racks failed; skipping rack mirror this cycle")
		return result
	}

	flowByExtID := make(map[string]*model.Rack, len(flowRacks))
	flowByNaturalKey := make(map[string]*model.Rack, len(flowRacks))
	for i := range flowRacks {
		r := &flowRacks[i]
		if r.ExternalID != nil && *r.ExternalID != "" {
			flowByExtID[*r.ExternalID] = r
		}
		if key := naturalKeyOrEmpty(r.Manufacturer, r.SerialNumber); key != "" {
			flowByNaturalKey[key] = r
		}
	}

	type plan struct {
		toInsert []model.Rack
		toUpdate []model.Rack
		toDelete []model.Rack
	}
	var p plan

	// seenExtID: every Core rack_id still reported this cycle, recorded
	// BEFORE any skip so the delete phase never drops a Flow rack Core is
	// still listing (even one whose labels are momentarily incomplete).
	// touchedIDs: Flow rack UUIDs the match path adopted / updated this
	// cycle; the delete phase skips them so a rack_id rename (update to the
	// new external_id) isn't immediately undone by a soft-delete keyed off
	// the stale in-memory external_id.
	seenExtID := make(map[string]struct{}, len(coreRacks))
	touchedIDs := make(map[uuid.UUID]struct{}, len(coreRacks))
	naturalKeyWinners := planRackNaturalKeyWinners(coreRacks, flowByNaturalKey)
	// coreExtIDs contains every rack_id in this response. It is precomputed
	// because natural-key adoption needs to know whether an external_id on a
	// candidate row is still authoritative even when that Core row appears
	// later in the response.
	coreExtIDs := make(map[string]struct{}, len(coreRacks))
	for _, cr := range coreRacks {
		if cr.RackID != "" {
			coreExtIDs[cr.RackID] = struct{}{}
		}
	}

	for _, cr := range coreRacks {
		// Record the rack_id as "still reported" before any skip below.
		if cr.RackID != "" {
			seenExtID[cr.RackID] = struct{}{}
		}

		built, ok := buildRackFromCore(cr)
		if !ok {
			log.Warn().
				Str("rack_profile_id", cr.RackProfileID).
				Str("name", cr.Name).
				Msg("Expected-inventory mirror: skipping Core expected rack with no rack_id; Flow has no identity to mirror it under")
			result.skippedNoIDOrKey++
			continue
		}

		// Two racks can't both store the same chassis pair —
		// rack_manufacturer_serial_idx would abort the cycle. Drop the labels
		// off the loser rather than the rack: external_id is its identity and
		// the labels are metadata. An incomplete pair is stored as NULL and
		// collides with nothing, so it needs no such check.
		naturalKey := naturalKeyOrEmpty(built.Manufacturer, built.SerialNumber)
		if naturalKey != "" {
			if winner := naturalKeyWinners[naturalKey]; winner != cr.RackID {
				log.Warn().
					Str("rack_id", cr.RackID).
					Str("winning_rack_id", winner).
					Str("manufacturer", built.Manufacturer).
					Str("serial", built.SerialNumber).
					Msg("Expected-inventory mirror: Core reported this chassis on more than one expected rack; mirroring the non-owner without chassis labels")
				built.Manufacturer = ""
				built.SerialNumber = ""
				naturalKey = ""
			}
		}

		// Prefer external_id match (already adopted on a previous cycle).
		if existing, ok := flowByExtID[cr.RackID]; ok {
			candidate := *existing
			needUpdate := false
			if candidate.DeletedAt != nil {
				candidate.DeletedAt = nil
				needUpdate = true
				result.resurrected++
			}
			if patched := rackUpdatedFromCore(&candidate, &built); patched != nil {
				candidate = *patched
				needUpdate = true
			}
			if needUpdate {
				p.toUpdate = append(p.toUpdate, candidate)
			}
			touchedIDs[existing.ID] = struct{}{}
			continue
		}

		// Fall back to the natural key to adopt rows the mirror has never
		// reached: those created before external_id existed, and those the
		// CreateExpectedRack gRPC creates without one. Adoption writes
		// external_id, so a row passes through here at most once. A match
		// that's also soft-deleted gets resurrected at the same time — see
		// the function-level comment for why this matters.
		if existing, ok := flowByNaturalKey[naturalKey]; ok && adoptableByNaturalKey(existing, coreExtIDs) {
			candidate := *existing
			candidate.ExternalID = built.ExternalID
			if candidate.DeletedAt != nil {
				candidate.DeletedAt = nil
				result.resurrected++
			}
			if patched := rackUpdatedFromCore(&candidate, &built); patched != nil {
				candidate = *patched
			}
			p.toUpdate = append(p.toUpdate, candidate)
			touchedIDs[existing.ID] = struct{}{}
			result.adopted++
			continue
		}

		p.toInsert = append(p.toInsert, built)
	}

	// Reconcile the delete side. Already soft-deleted rows are skipped: if
	// Core still lists them, the match path above resurrected them; if not,
	// they're correctly gone already. Live Flow rows whose external_id is set
	// but absent from Core get soft-deleted. Every remaining live legacy row
	// without external_id is also deleted: a complete natural-key match would
	// already have been adopted above, so the rest are absent from the
	// authoritative snapshot.
	for i := range flowRacks {
		r := &flowRacks[i]
		if r.DeletedAt != nil {
			continue
		}
		// Skip rows the match path already adopted / updated this cycle. A
		// rack_id rename updates the row to the new external_id; without this
		// guard the delete phase would see the stale in-memory external_id
		// (not in seenExtID) and soft-delete the row we just renamed.
		if _, touched := touchedIDs[r.ID]; touched {
			continue
		}
		hasExt := r.ExternalID != nil && *r.ExternalID != ""
		if hasExt {
			if _, present := seenExtID[*r.ExternalID]; present {
				continue
			}
			p.toDelete = append(p.toDelete, *r)
			continue
		}
		p.toDelete = append(p.toDelete, *r)
	}

	if len(p.toInsert) == 0 && len(p.toUpdate) == 0 && len(p.toDelete) == 0 {
		return result
	}

	now := time.Now()
	softDeleted := 0
	if err := pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		for i := range p.toInsert {
			if chassisSlotOwnedByOther(flowByNaturalKey, &p.toInsert[i]) {
				if err := releaseChassisSlot(ctx, tx, &p.toInsert[i], now); err != nil {
					return err
				}
			}
			insertResult, err := tx.NewInsert().Model(&p.toInsert[i]).Exec(ctx)
			if err != nil {
				return fmt.Errorf("insert rack %q: %w", p.toInsert[i].Name, err)
			}
			rowsAffected, err := insertResult.RowsAffected()
			if err != nil {
				return fmt.Errorf("count inserted rack %q: %w", p.toInsert[i].Name, err)
			}
			if rowsAffected != 1 {
				return fmt.Errorf("insert rack %q affected %d rows, expected 1", p.toInsert[i].Name, rowsAffected)
			}
		}
		for i := range p.toUpdate {
			if chassisSlotOwnedByOther(flowByNaturalKey, &p.toUpdate[i]) {
				if err := releaseChassisSlot(ctx, tx, &p.toUpdate[i], now); err != nil {
					return err
				}
			}
			// Mirror-managed columns only; status / health / ingested_at / nvldomain_id
			// belong to other paths. WhereAllWithDeleted is required so a
			// resurrection (deleted_at cleared in Go) matches the tombstone —
			// bun otherwise appends "deleted_at IS NULL" to the UPDATE and the
			// resurrect silently matches zero rows.
			p.toUpdate[i].UpdatedAt = now
			updateResult, err := tx.NewUpdate().
				Model(&p.toUpdate[i]).
				Column("name", "manufacturer", "serial_number", "description",
					"location", "external_id", "rack_profile_id", "deleted_at", "updated_at").
				WhereAllWithDeleted().
				Where("id = ?", p.toUpdate[i].ID).
				Exec(ctx)
			if err != nil {
				return fmt.Errorf("update rack %q: %w", p.toUpdate[i].Name, err)
			}
			rowsAffected, err := updateResult.RowsAffected()
			if err != nil {
				return fmt.Errorf("count updated rack %q: %w", p.toUpdate[i].Name, err)
			}
			if rowsAffected != 1 {
				return fmt.Errorf("update rack %q affected %d rows, expected 1", p.toUpdate[i].Name, rowsAffected)
			}
		}
		for i := range p.toDelete {
			deleteResult, err := tx.NewDelete().Model(&p.toDelete[i]).Where("id = ?", p.toDelete[i].ID).Exec(ctx)
			if err != nil {
				return fmt.Errorf("soft-delete rack %q: %w", p.toDelete[i].Name, err)
			}
			rowsAffected, err := deleteResult.RowsAffected()
			if err != nil {
				return fmt.Errorf("count soft-deleted rack %q: %w", p.toDelete[i].Name, err)
			}
			if rowsAffected != 1 {
				return fmt.Errorf("soft-delete rack %q affected %d rows, expected 1", p.toDelete[i].Name, rowsAffected)
			}
			softDeleted += int(rowsAffected)
		}
		return nil
	}); err != nil {
		log.Error().Err(err).Msg("Expected-inventory mirror: rack reconciliation transaction failed; mirror is no-op this cycle")
		// Tx rolled back: per-spec decisions logged above describe intent,
		// not committed state. Strip success-side counters so the summary
		// log reflects what actually landed (nothing). pulled,
		// skippedNoIDOrKey survives: it is decided before the tx opened and
		// isn't invalidated by the rollback.
		result.resurrected = 0
		result.adopted = 0
		return result
	}

	result.inserted = len(p.toInsert)
	result.updated = len(p.toUpdate)
	result.softDeleted = softDeleted
	return result
}

// chassisSlotOwnedByOther reports whether the initial Flow snapshot contains
// another row holding the desired chassis pair. The transaction only needs a
// release UPDATE in that case. This remains correct for swaps: both desired
// pairs are occupied by another row in the initial snapshot, so both release
// operations are planned before their corresponding authoritative updates.
func chassisSlotOwnedByOther(
	flowByNaturalKey map[string]*model.Rack,
	desired *model.Rack,
) bool {
	key := naturalKeyOrEmpty(desired.Manufacturer, desired.SerialNumber)
	if key == "" {
		return false
	}
	owner := flowByNaturalKey[key]
	return owner != nil && owner.ID != desired.ID
}

// getAllRacksIncludingDeleted returns every rack in the Flow DB, soft-deleted
// rows included. The mirror needs the deleted ones so it can (a) resurrect a
// rack that comes back in Core instead of attempting an INSERT that would
// collide on the (manufacturer, serial_number) unique index the tombstone
// still holds, and (b) not double-delete a row that's already gone.
func getAllRacksIncludingDeleted(ctx context.Context, idb bun.IDB) ([]model.Rack, error) {
	var racks []model.Rack
	if err := idb.NewSelect().Model(&racks).WhereAllWithDeleted().Scan(ctx); err != nil {
		return nil, err
	}
	return racks, nil
}

// buildRackFromCore translates one Core ExpectedRackDetail into the Flow Rack
// shape the mirror will insert. Returns false when Core supplied no rack_id:
// external_id is the mirrored rack's identity, so without one the row could
// not be matched again on any later cycle. Core's expected_racks keys on
// rack_id, so this is a Core-side data fault rather than an expected input.
//
// The chassis labels are copied through as-is, empty included. They are
// descriptive metadata here, not identity. The planner degrades duplicate
// pairs within one Core response, and the transaction releases any stale Flow
// holder before the external-id row claims its desired pair.
func buildRackFromCore(cr nicoapi.ExpectedRackDetail) (model.Rack, bool) {
	if cr.RackID == "" {
		return model.Rack{}, false
	}

	name := cr.Name
	if name == "" {
		// Flow's rack.name is NOT NULL. Core's rack_id is operator-meaningful,
		// so use it as the display name when Core has no name metadata.
		name = cr.RackID
	}

	extID := cr.RackID
	r := model.Rack{
		Name:         name,
		Manufacturer: cr.Labels[labelChassisManufacturer],
		SerialNumber: cr.Labels[labelChassisSerialNumber],
		ExternalID:   &extID,
	}
	if cr.RackProfileID != "" {
		r.RackProfileID = &cr.RackProfileID
	}

	if desc := rackDescriptionFromLabels(cr.Labels, cr.Description); len(desc) > 0 {
		r.Description = desc
	}
	if loc := rackLocationFromLabels(cr.Labels); len(loc) > 0 {
		r.Location = loc
	}
	return r, true
}

// rackDescriptionFromLabels extracts the JSONB-bound description fields the
// existing GetListOfRacks filter knows about (currently just "model") and
// preserves Core's free-form description text under "text". Returns an empty
// map when there's nothing to record so the caller can leave Description as
// SQL NULL.
func rackDescriptionFromLabels(labels map[string]string, description string) map[string]any {
	out := map[string]any{}
	if v := labels[labelChassisModel]; v != "" {
		out["model"] = v
	}
	if description != "" {
		out["text"] = description
	}
	return out
}

// rackLocationFromLabels extracts the well-known location.* labels into the
// JSONB Location column. Returns an empty map when none are present.
func rackLocationFromLabels(labels map[string]string) map[string]any {
	out := map[string]any{}
	if v := labels[labelLocationRegion]; v != "" {
		out["region"] = v
	}
	if v := labels[labelLocationDatacenter]; v != "" {
		out["data_center"] = v
	}
	if v := labels[labelLocationRoom]; v != "" {
		out["room"] = v
	}
	if v := labels[labelLocationPosition]; v != "" {
		out["position"] = v
	}
	return out
}

// rackUpdatedFromCore returns a copy of `existing` with mirror-managed fields
// overwritten from `fromCore`. Runtime health, lifecycle (status / ingested_at),
// and nvldomain_id belong to other paths and are left alone.
//
// A successful expected-inventory response is authoritative for all fields
// copied by buildRackFromCore. Missing labels and metadata therefore clear the
// corresponding Flow values. Runtime-owned fields are not part of fromCore and
// remain untouched.
//
// Returns nil when no patchable field changed so the caller can skip a no-op
// UPDATE.
func rackUpdatedFromCore(existing, fromCore *model.Rack) *model.Rack {
	patched := *existing
	changed := false
	if !reflect.DeepEqual(existing.RackProfileID, fromCore.RackProfileID) {
		patched.RackProfileID = fromCore.RackProfileID
		changed = true
	}

	if fromCore.Name != "" && existing.Name != fromCore.Name {
		patched.Name = fromCore.Name
		changed = true
	}
	if !reflect.DeepEqual(existing.Description, fromCore.Description) {
		patched.Description = fromCore.Description
		changed = true
	}
	if !reflect.DeepEqual(existing.Location, fromCore.Location) {
		patched.Location = fromCore.Location
		changed = true
	}
	// Adopt: existing.ExternalID was nil but fromCore now provides one.
	if (existing.ExternalID == nil || *existing.ExternalID == "") && fromCore.ExternalID != nil && *fromCore.ExternalID != "" {
		patched.ExternalID = fromCore.ExternalID
		changed = true
	}
	if existing.Manufacturer != fromCore.Manufacturer {
		patched.Manufacturer = fromCore.Manufacturer
		changed = true
	}
	if existing.SerialNumber != fromCore.SerialNumber {
		patched.SerialNumber = fromCore.SerialNumber
		changed = true
	}

	if !changed {
		return nil
	}
	return &patched
}

// adoptableByNaturalKey reports whether a natural-key match may take over the
// Flow row and write Core's rack_id onto it. A row carrying no external_id has
// never been claimed, so it is always adoptable. A row that carries one may only
// be re-pointed once Core has stopped reporting that rack_id, which is how a
// rack re-registered under a new rack_id keeps its UUID. While both rack_ids are
// live the row belongs to the one already named on it: taking it would leave the
// other Core rack with no row of its own and make the two trade this one on
// every cycle.
func adoptableByNaturalKey(r *model.Rack, coreExtIDs map[string]struct{}) bool {
	if r.ExternalID == nil || *r.ExternalID == "" {
		return true
	}
	_, stillReported := coreExtIDs[*r.ExternalID]
	return !stillReported
}

// planRackNaturalKeyWinners chooses one Core rack for each duplicated chassis
// pair that Flow's unique index can represent only once. An external-id row
// already holding the pair keeps it when that rack still claims it; otherwise
// the lexicographically smallest rack_id wins so Core response order cannot
// change ownership between reconciliation cycles.
func planRackNaturalKeyWinners(
	coreRacks []nicoapi.ExpectedRackDetail,
	flowByNaturalKey map[string]*model.Rack,
) map[string]string {
	claimants := make(map[string]map[string]struct{}, len(coreRacks))
	for _, coreRack := range coreRacks {
		if coreRack.RackID == "" {
			continue
		}
		key := naturalKeyOrEmpty(
			coreRack.Labels[labelChassisManufacturer],
			coreRack.Labels[labelChassisSerialNumber],
		)
		if key == "" {
			continue
		}
		if claimants[key] == nil {
			claimants[key] = make(map[string]struct{})
		}
		claimants[key][coreRack.RackID] = struct{}{}
	}

	winners := make(map[string]string, len(claimants))
	for key, rackIDs := range claimants {
		winner := ""
		for rackID := range rackIDs {
			if winner == "" || rackID < winner {
				winner = rackID
			}
		}
		if owner := flowByNaturalKey[key]; owner != nil && owner.ExternalID != nil {
			if _, stillClaimsPair := rackIDs[*owner.ExternalID]; stillClaimsPair {
				winner = *owner.ExternalID
			}
		}
		winners[key] = winner
	}
	return winners
}

// releaseChassisSlot clears a desired chassis pair from any other Flow row
// before an INSERT or UPDATE claims it. external_id is the authoritative rack
// identity, so a stale natural-key holder must not prevent Core from correcting
// the external-id row. Clearing first also permits two racks to swap chassis
// pairs within one transaction. The unique index guarantees at most one other
// row can be changed.
func releaseChassisSlot(
	ctx context.Context,
	tx bun.Tx,
	desired *model.Rack,
	now time.Time,
) error {
	if naturalKeyOrEmpty(desired.Manufacturer, desired.SerialNumber) == "" {
		return nil
	}

	query := tx.NewUpdate().
		Model(&model.Rack{}).
		Set("manufacturer = NULL").
		Set("serial_number = NULL").
		Set("updated_at = ?", now).
		Where("manufacturer = ?", desired.Manufacturer).
		Where("serial_number = ?", desired.SerialNumber).
		WhereAllWithDeleted()
	if desired.ID != uuid.Nil {
		query = query.Where("id <> ?", desired.ID)
	}

	updateResult, err := query.Exec(ctx)
	if err != nil {
		return fmt.Errorf("release chassis slot for rack %q: %w", desired.Name, err)
	}
	rowsAffected, err := updateResult.RowsAffected()
	if err != nil {
		return fmt.Errorf("count released chassis slots for rack %q: %w", desired.Name, err)
	}
	if rowsAffected > 1 {
		return fmt.Errorf("release chassis slot for rack %q affected %d rows, expected at most 1", desired.Name, rowsAffected)
	}
	if rowsAffected == 1 {
		rackID := ""
		if desired.ExternalID != nil {
			rackID = *desired.ExternalID
		}
		log.Info().
			Str("rack_id", rackID).
			Str("manufacturer", desired.Manufacturer).
			Str("serial", desired.SerialNumber).
			Msg("Expected-inventory mirror: released occupied chassis slot for authoritative rack update")
	}
	return nil
}
