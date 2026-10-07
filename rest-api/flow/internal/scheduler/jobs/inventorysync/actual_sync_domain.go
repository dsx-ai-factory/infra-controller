// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"fmt"
	"sort"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
)

// domainMirrorResult summarizes one committed actual domain-topology reconciliation.
type domainMirrorResult struct {
	pulled              int
	domainsInserted     int
	domainsResurrected  int
	domainsSoftDeleted  int
	membershipsAssigned int
}

type domainTopologySnapshot struct {
	groupByRack    map[uuid.UUID]string
	clusterByGroup map[string]*uuid.UUID
	invalidGroups  map[string]bool
}

func (r domainMirrorResult) log() {
	log.Info().
		Str("resource", "nvlink_domains").
		Int("pulled", r.pulled).
		Int("domains_inserted", r.domainsInserted).
		Int("domains_resurrected", r.domainsResurrected).
		Int("domains_soft_deleted", r.domainsSoftDeleted).
		Int("memberships_assigned", r.membershipsAssigned).
		Msg("Actual-inventory sync: NVLink domains")
}

func pullObservedNVLinkDomainMemberships(
	ctx context.Context,
	nicoClient nicoapi.Client,
) (rows []nicoapi.NVLinkDomainMembership, rpcOK bool) {
	rows, err := nicoClient.GetObservedNVLinkDomainMemberships(ctx)
	if err != nil {
		log.Error().Err(err).
			Msg("Actual-inventory sync: pulling observed NVLink domain memberships failed; preserving existing topology")
		return nil, false
	}
	return rows, true
}

func syncObservedNVLinkDomainTopology(
	ctx context.Context,
	pool *cdb.Session,
	nicoClient nicoapi.Client,
) {
	memberships, membershipsOK := pullObservedNVLinkDomainMemberships(ctx, nicoClient)
	if !membershipsOK {
		return
	}

	rackIDByExternalID, err := loadRackIDByExternalID(ctx, pool.DB)
	if err != nil {
		log.Error().Err(err).
			Msg("Actual-inventory sync: loading rack external_id map failed; preserving existing NVLink domain topology")
		return
	}

	rackExternalIDs := make([]string, 0, len(rackIDByExternalID))
	for externalID := range rackIDByExternalID {
		rackExternalIDs = append(rackExternalIDs, externalID)
	}
	sort.Strings(rackExternalIDs)
	groups, err := nicoClient.FindRackGroupIDs(ctx, rackExternalIDs)
	if err != nil {
		log.Error().Err(err).Msg("Actual-inventory sync: pulling rack groups failed; preserving domain topology")
		return
	}
	result, err := mirrorObservedNVLinkDomainMemberships(ctx, pool, memberships, rackIDByExternalID, groups)
	if err != nil {
		log.Error().Err(err).
			Msg("Actual-inventory sync: NVLink domain reconciliation failed; preserving existing topology")
		return
	}
	result.log()
}

// mirrorObservedNVLinkDomainMemberships reconciles a complete observed
// domain-topology snapshot into Flow. rackIDByExternalID translates inventory
// rack IDs into Flow rack UUIDs.
//
// Persisted rack groups own membership independently of switch observations.
// Missing groups preserve legacy membership; absent observations clear only the
// NMX-C cluster. Invalid or conflicting observations preserve that group's cluster.
// Domain rows and rack memberships are committed together. A
// displaced domain is soft-deleted only when no active rack references it;
// unrelated unreferenced domains are preserved because Flow supports manual
// domain creation. Observations for racks absent from active Flow inventory are
// skipped with a warning without invalidating observations for known racks.
func mirrorObservedNVLinkDomainMemberships(
	ctx context.Context,
	pool *cdb.Session,
	memberships []nicoapi.NVLinkDomainMembership,
	rackIDByExternalID map[string]uuid.UUID,
	groupByRackExternalID map[string]string,
) (domainMirrorResult, error) {
	result := domainMirrorResult{pulled: len(memberships)}
	snapshot := buildDomainTopologySnapshot(memberships, rackIDByExternalID, groupByRackExternalID)

	err := pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		var existingDomains []model.NVLDomain
		err := tx.NewSelect().
			Model(&existingDomains).
			WhereAllWithDeleted().
			Scan(ctx)
		if err != nil {
			return fmt.Errorf("load existing NVLink domains: %w", err)
		}

		existingByGroup := make(map[string]*model.NVLDomain, len(existingDomains))
		for i := range existingDomains {
			if existingDomains[i].ExternalID != nil {
				existingByGroup[*existingDomains[i].ExternalID] = &existingDomains[i]
			}
		}

		rackIDs := sortedUUIDValues(rackIDByExternalID)
		currentDomainByRack := make(map[uuid.UUID]uuid.UUID, len(rackIDs))
		if len(rackIDs) > 0 {
			var currentRacks []model.Rack
			err = tx.NewSelect().
				Model(&currentRacks).
				Column("id", "nvldomain_id").
				Where("id IN (?)", bun.In(rackIDs)).
				Scan(ctx)
			if err != nil {
				return fmt.Errorf("load current rack NVLink domain memberships: %w", err)
			}
			for i := range currentRacks {
				currentDomainByRack[currentRacks[i].ID] = currentRacks[i].NVLDomainID
			}
		}

		groupIDs := make([]string, 0, len(snapshot.clusterByGroup))
		for groupID := range snapshot.clusterByGroup {
			groupIDs = append(groupIDs, groupID)
		}
		sort.Strings(groupIDs)
		domainIDByGroup := make(map[string]uuid.UUID, len(groupIDs))
		for _, groupID := range groupIDs {
			existing, found := existingByGroup[groupID]
			if !found {
				// Core reports group identity, not a domain display name.
				domain := model.NVLDomain{ID: uuid.New(), ExternalID: &groupID, NMXCClusterID: snapshot.clusterByGroup[groupID]}
				insertResult, insertErr := tx.NewInsert().Model(&domain).Exec(ctx)
				if insertErr != nil {
					return fmt.Errorf("insert NVLink domain %s: %w", groupID, insertErr)
				}
				changed, rowsErr := insertResult.RowsAffected()
				if rowsErr != nil {
					return rowsErr
				}
				result.domainsInserted += int(changed)
				domainIDByGroup[groupID] = domain.ID
				continue
			}
			domainID := existing.ID
			domainIDByGroup[groupID] = domainID
			if !snapshot.invalidGroups[groupID] {
				_, err = tx.NewUpdate().Model(existing).WhereAllWithDeleted().Where("id = ?", domainID).
					Set("nmxc_cluster_id = ?", snapshot.clusterByGroup[groupID]).Exec(ctx)
				if err != nil {
					return fmt.Errorf("update NMX-C cluster for group %s: %w", groupID, err)
				}
			}

			if existing.DeletedAt == nil {
				continue
			}

			updateResult, updateErr := tx.NewUpdate().
				Model(existing).
				Set("deleted_at = NULL").
				WhereAllWithDeleted().
				Where("id = ?", domainID).
				Where("deleted_at IS NOT NULL").
				Exec(ctx)
			if updateErr != nil {
				return fmt.Errorf("restore NVLink domain %s: %w", domainID, updateErr)
			}
			changed, rowsErr := updateResult.RowsAffected()
			if rowsErr != nil {
				return fmt.Errorf("count restored NVLink domains for %s: %w", domainID, rowsErr)
			}
			result.domainsResurrected += int(changed)
		}

		now := time.Now()
		displacedDomainIDs := make(map[uuid.UUID]struct{})
		for _, rackID := range rackIDs {
			groupID, assigned := snapshot.groupByRack[rackID]
			if !assigned {
				// Undiscovered and legacy racks have no authoritative group yet.
				continue
			}
			domainID := domainIDByGroup[groupID]
			currentDomainID := currentDomainByRack[rackID]
			if currentDomainID != uuid.Nil && currentDomainID != domainID {
				displacedDomainIDs[currentDomainID] = struct{}{}
			}
			query := tx.NewUpdate().
				Model((*model.Rack)(nil)).
				Set("updated_at = ?", now).
				Where("id = ?", rackID).
				Set("nvldomain_id = ?", domainID).
				Set("rack_group_id = ?", groupID).
				Where("(nvldomain_id IS DISTINCT FROM ? OR rack_group_id IS DISTINCT FROM ?)", domainID, groupID)

			updateResult, updateErr := query.Exec(ctx)
			if updateErr != nil {
				return fmt.Errorf("reconcile NVLink domain for rack %s: %w", rackID, updateErr)
			}
			changed, rowsErr := updateResult.RowsAffected()
			if rowsErr != nil {
				return fmt.Errorf("count NVLink domain changes for rack %s: %w", rackID, rowsErr)
			}
			if changed == 0 {
				continue
			}
			result.membershipsAssigned += int(changed)
		}

		var referencedDomainIDs []uuid.UUID
		err = tx.NewSelect().
			Model((*model.Rack)(nil)).
			Column("nvldomain_id").
			Where("nvldomain_id IS NOT NULL").
			Group("nvldomain_id").
			Scan(ctx, &referencedDomainIDs)
		if err != nil {
			return fmt.Errorf("load active rack NVLink domain references: %w", err)
		}
		referencedDomainIDSet := make(map[uuid.UUID]struct{}, len(referencedDomainIDs))
		for _, domainID := range referencedDomainIDs {
			referencedDomainIDSet[domainID] = struct{}{}
		}

		for i := range existingDomains {
			existing := &existingDomains[i]
			if existing.DeletedAt != nil {
				continue
			}
			_, displaced := displacedDomainIDs[existing.ID]
			if !displaced {
				continue
			}
			_, referenced := referencedDomainIDSet[existing.ID]
			if referenced {
				continue
			}

			deleteResult, deleteErr := tx.NewDelete().
				Model(existing).
				Where("id = ?", existing.ID).
				Exec(ctx)
			if deleteErr != nil {
				return fmt.Errorf("soft-delete stale NVLink domain %s: %w", existing.ID, deleteErr)
			}
			changed, rowsErr := deleteResult.RowsAffected()
			if rowsErr != nil {
				return fmt.Errorf("count stale NVLink domain deletes for %s: %w", existing.ID, rowsErr)
			}
			result.domainsSoftDeleted += int(changed)
		}

		return nil
	})
	if err != nil {
		return domainMirrorResult{pulled: len(memberships)}, err
	}

	return result, nil
}

func buildDomainTopologySnapshot(
	memberships []nicoapi.NVLinkDomainMembership,
	rackIDByExternalID map[string]uuid.UUID,
	groupByRackExternalID map[string]string,
) domainTopologySnapshot {
	snapshot := domainTopologySnapshot{groupByRack: make(map[uuid.UUID]string), clusterByGroup: make(map[string]*uuid.UUID), invalidGroups: make(map[string]bool)}
	for externalID, rackID := range rackIDByExternalID {
		groupID := groupByRackExternalID[externalID]
		if groupID != "" {
			snapshot.groupByRack[rackID] = groupID
			snapshot.clusterByGroup[groupID] = nil
		}
	}
	for _, membership := range memberships {
		rackID, known := rackIDByExternalID[membership.RackID]
		groupID := snapshot.groupByRack[rackID]
		if !known || groupID == "" {
			log.Warn().Str("rack_id", membership.RackID).Msg("Skipping NMX-C observation without a known rack group")
			continue
		}
		clusterID, err := uuid.Parse(membership.DomainID)
		current := snapshot.clusterByGroup[groupID]
		if err != nil || clusterID == uuid.Nil || (current != nil && *current != clusterID) {
			log.Error().Str("rack_group_id", groupID).Str("nmxc_cluster_id", membership.DomainID).
				Msg("Invalid or conflicting NMX-C cluster observation; preserving this group's cluster")
			snapshot.invalidGroups[groupID] = true
		}
		if !snapshot.invalidGroups[groupID] {
			snapshot.clusterByGroup[groupID] = &clusterID
		}
	}
	for groupID := range snapshot.invalidGroups {
		snapshot.clusterByGroup[groupID] = nil
	}
	return snapshot
}

func sortedUUIDValues(values map[string]uuid.UUID) []uuid.UUID {
	ids := make([]uuid.UUID, 0, len(values))
	for _, id := range values {
		ids = append(ids, id)
	}
	sort.Slice(ids, func(i, j int) bool { return ids[i].String() < ids[j].String() })
	return ids
}
