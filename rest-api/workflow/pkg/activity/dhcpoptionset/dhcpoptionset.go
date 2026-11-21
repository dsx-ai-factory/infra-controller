// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dhcpoptionset

import (
	"context"
	"errors"
	"fmt"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageDhcpOptionSet is an activity wrapper for DhcpOptionSet inventory that allows injecting DB access
type ManageDhcpOptionSet struct {
	dbSession *cdb.Session
}

// UpdateDhcpOptionSetsInDB is a Temporal activity that takes DhcpOptionSet inventory pushed by Site Agent
// and updates the DB. Sets the Site reports are refreshed, or created when REST has no row for them;
// sets the Site no longer reports are flagged as missing so the Tenant can decide on deletion.
func (mdos ManageDhcpOptionSet) UpdateDhcpOptionSetsInDB(ctx context.Context, siteID uuid.UUID, inventory *corev1.DhcpOptionSetInventory) error {
	logger := log.With().Str("Activity", "UpdateDhcpOptionSetsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if inventory == nil {
		logger.Error().Msg("UpdateDhcpOptionSetsInDB called with nil inventory")
		return errors.New("UpdateDhcpOptionSetsInDB called with nil inventory")
	}

	site, err := cdbm.NewSiteDAO(mdos.dbSession).GetByID(ctx, nil, siteID, nil, false, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Warn().Err(err).Msg("received DHCP Option Set inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	if inventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	dosDAO := cdbm.NewDhcpOptionSetDAO(mdos.dbSession)

	existingDOSs, _, err := dosDAO.GetAll(ctx, nil, cdbm.DhcpOptionSetFilterInput{
		SiteIDs: []uuid.UUID{site.ID},
	}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get DHCP Option Sets for Site from DB")
		return err
	}

	// Core creates each set under the ID REST supplied, so the REST ID is also the ID the Site reports.
	existingDOSIDMap := make(map[uuid.UUID]*cdbm.DhcpOptionSet, len(existingDOSs))
	for i := range existingDOSs {
		existingDOSIDMap[existingDOSs[i].ID] = &existingDOSs[i]
	}

	reportedDOSIDMap := map[uuid.UUID]bool{}
	if inventory.InventoryPage != nil {
		for _, strID := range inventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strID)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strID).Msg("failed to parse DHCP Option Set ID from inventory page")
				continue
			}
			reportedDOSIDMap[id] = true
		}
	}

	for _, coreDOS := range inventory.DhcpOptionSets {
		slogger := logger.With().Str("DHCP Option Set ID", coreDOS.GetId().GetValue()).Logger()

		id, serr := uuid.Parse(coreDOS.GetId().GetValue())
		if serr != nil {
			slogger.Warn().Err(serr).Msg("skipping DHCP Option Set with an ID that is not a valid UUID")
			continue
		}
		reportedDOSIDMap[id] = true

		dos, ok := existingDOSIDMap[id]
		if !ok {
			// Values come straight from Core, so a created or restored set needs no further refresh.
			created := mdos.createOrRestoreDhcpOptionSetFromSite(ctx, site, id, coreDOS)
			if created != nil {
				existingDOSIDMap[id] = created
			}
			continue
		}

		// A row written since the Site collected this inventory holds changes the snapshot
		// cannot know about, so writing the reported values over them would lose those edits.
		if site.IsTimeWithinStaleInventoryThreshold(dos.Updated) {
			slogger.Info().Msg("not updating DHCP Option Set yet because it changed more recently than the inventory interval")
			continue
		}

		var update *cdbm.DhcpOptionSetUpdateInput
		if dos.Version != coreDOS.GetVersion() {
			update = updateInputFromSite(dos.ID, coreDOS)
		}
		if dos.IsMissingOnSite {
			if update == nil {
				update = &cdbm.DhcpOptionSetUpdateInput{DhcpOptionSetID: dos.ID}
			}
			update.IsMissingOnSite = cwutil.GetPtr(false)
		}
		if update != nil {
			_, serr = dosDAO.Update(ctx, nil, *update)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update DHCP Option Set from Site inventory")
				continue
			}
		}

		// Being reported by the Site is what makes a set Ready again after it went missing.
		if dos.Status != cdbm.DhcpOptionSetStatusReady {
			serr = cdb.WithTx(ctx, mdos.dbSession, func(tx *cdb.Tx) error {
				return mdos.updateDhcpOptionSetStatusInDB(ctx, tx, dos.ID, cdbm.DhcpOptionSetStatusReady, "DHCP Option Set was found on Site")
			})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update DHCP Option Set status in DB")
			}
		}
	}

	// With inventory paging, deletions are reconciled once, on the last page.
	if !util.ShouldReconcileDeletions(inventory.GetInventoryPage()) {
		return nil
	}

	for i := range existingDOSs {
		dos := &existingDOSs[i]
		if reportedDOSIDMap[dos.ID] {
			continue
		}

		// A set created within the inventory interval may postdate this inventory.
		if site.IsTimeWithinStaleInventoryThreshold(dos.Created) {
			continue
		}

		// Already recorded as missing, so re-applying it would only append a duplicate status detail.
		if dos.IsMissingOnSite && dos.Status == cdbm.DhcpOptionSetStatusError {
			continue
		}

		serr := cdb.WithTx(ctx, mdos.dbSession, func(tx *cdb.Tx) error {
			_, derr := dosDAO.Update(ctx, tx, cdbm.DhcpOptionSetUpdateInput{
				DhcpOptionSetID: dos.ID,
				IsMissingOnSite: cwutil.GetPtr(true),
			})
			if derr != nil {
				return derr
			}
			return mdos.updateDhcpOptionSetStatusInDB(ctx, tx, dos.ID, cdbm.DhcpOptionSetStatusError, "DHCP Option Set is missing on Site")
		})
		if serr != nil {
			logger.Error().Err(serr).Str("DHCP Option Set ID", dos.ID.String()).Msg("failed to record DHCP Option Set as missing on Site")
		}
	}

	return nil
}

// createOrRestoreDhcpOptionSetFromSite creates a REST DhcpOptionSet for a set the Site reports but REST
// has no active row for, or restores a matching soft-deleted row. Returns nil when skipped or on failure.
func (mdos ManageDhcpOptionSet) createOrRestoreDhcpOptionSetFromSite(ctx context.Context, site *cdbm.Site, id uuid.UUID, coreDOS *corev1.DhcpOptionSet) *cdbm.DhcpOptionSet {
	logger := log.With().Str("Activity", "UpdateDhcpOptionSetsInDB").Str("Site ID", site.ID.String()).Str("DHCP Option Set ID", id.String()).Logger()

	org := coreDOS.GetTenantOrganizationId()
	if org == "" {
		logger.Warn().Msg("unable to create DHCP Option Set found on Site: it reports an empty Tenant organization ID")
		return nil
	}

	dos, err := cdb.WithTxResult(ctx, mdos.dbSession, func(tx *cdb.Tx) (*cdbm.DhcpOptionSet, error) {
		dosDAO := cdbm.NewDhcpOptionSetDAO(mdos.dbSession)

		matches, _, derr := dosDAO.GetAll(ctx, tx, cdbm.DhcpOptionSetFilterInput{
			DhcpOptionSetIDs: []uuid.UUID{id}, SiteIDs: []uuid.UUID{site.ID}, IncludeDeleted: true,
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if derr != nil {
			return nil, fmt.Errorf("failed to retrieve DHCP Option Set by ID: %w", derr)
		}

		if len(matches) > 0 {
			existing := &matches[0]
			if existing.Deleted == nil {
				return existing, nil
			}
			if existing.TenantOrg != org {
				logger.Warn().Msg("unable to restore DHCP Option Set found on Site: Tenant organization differs from the REST record")
				return nil, nil
			}
			// A delete newer than the inventory interval can postdate this inventory, so restoring
			// then would revive a set the snapshot never saw removed.
			if site.IsTimeWithinStaleInventoryThreshold(*existing.Deleted) {
				logger.Info().Msg("not restoring DHCP Option Set yet because it was deleted more recently than the inventory interval")
				return nil, nil
			}

			_, derr = dosDAO.Clear(ctx, tx, cdbm.DhcpOptionSetClearInput{DhcpOptionSetID: id, Deleted: true})
			if derr != nil {
				return nil, fmt.Errorf("failed to restore DHCP Option Set: %w", derr)
			}
			update := updateInputFromSite(id, coreDOS)
			update.IsMissingOnSite = cwutil.GetPtr(false)
			_, derr = dosDAO.Update(ctx, tx, *update)
			if derr != nil {
				return nil, fmt.Errorf("failed to refresh restored DHCP Option Set: %w", derr)
			}
			derr = mdos.updateDhcpOptionSetStatusInDB(ctx, tx, id, cdbm.DhcpOptionSetStatusReady, "DHCP Option Set was found on Site")
			if derr != nil {
				return nil, fmt.Errorf("failed to update status of restored DHCP Option Set: %w", derr)
			}
			return dosDAO.GetByID(ctx, tx, id, nil)
		}

		tenants, _, derr := cdbm.NewTenantDAO(mdos.dbSession).GetAll(ctx, tx, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if derr != nil {
			return nil, fmt.Errorf("failed to retrieve Tenant by organization: %w", derr)
		}
		if len(tenants) == 0 {
			logger.Warn().Str("Org", org).Msg("unable to create DHCP Option Set found on Site: no Tenant found for its organization")
			return nil, nil
		}
		tenant := &tenants[0]

		fromSite := &cdbm.DhcpOptionSet{}
		fromSite.FromProto(coreDOS)

		// REST keeps names unique per Tenant and Site, so a recovered set that collides gets a suffix.
		name := fromSite.Name
		if name == "" {
			name = fmt.Sprintf("recovered-%s", id.String()[:8])
		}
		conflicts, _, derr := dosDAO.GetAll(ctx, tx, cdbm.DhcpOptionSetFilterInput{
			Names: []string{name}, TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{site.ID},
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if derr != nil {
			return nil, fmt.Errorf("failed to retrieve DHCP Option Set by name: %w", derr)
		}
		if len(conflicts) > 0 {
			name = fmt.Sprintf("%s-recovered-%s", name, id.String()[:8])
		}

		created, derr := dosDAO.Create(ctx, tx, cdbm.DhcpOptionSetCreateInput{
			DhcpOptionSetID: &id,
			Name:            name,
			Description:     fromSite.Description,
			TenantOrg:       org,
			SiteID:          site.ID,
			TenantID:        tenant.ID,
			Options:         fromSite.Options,
			Labels:          fromSite.Labels,
			Status:          cdbm.DhcpOptionSetStatusReady,
			Version:         cwutil.GetPtr(coreDOS.GetVersion()),
			CreatedBy:       tenant.CreatedBy,
		})
		if derr != nil {
			return nil, fmt.Errorf("failed to create DHCP Option Set: %w", derr)
		}

		_, derr = cdbm.NewStatusDetailDAO(mdos.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: created.ID.String(),
			Status:   string(cdbm.DhcpOptionSetStatusReady),
			Message:  cwutil.GetPtr("DHCP Option Set was found on Site"),
		})
		if derr != nil {
			return nil, fmt.Errorf("failed to create Status Detail: %w", derr)
		}
		return created, nil
	})
	if err != nil {
		logger.Warn().Err(err).Msg("failed to create or restore DHCP Option Set from Site inventory")
		return nil
	}
	if dos != nil {
		logger.Info().Msg("created or restored DHCP Option Set from Site inventory")
	}
	return dos
}

// updateInputFromSite builds an update that replaces the stored values with the ones the Site reports
func updateInputFromSite(id uuid.UUID, coreDOS *corev1.DhcpOptionSet) *cdbm.DhcpOptionSetUpdateInput {
	fromSite := &cdbm.DhcpOptionSet{}
	fromSite.FromProto(coreDOS)

	// An empty description or label list from the Site clears the stored value.
	labels := map[string]string{}
	for k, v := range fromSite.Labels {
		labels[k] = v
	}
	return &cdbm.DhcpOptionSetUpdateInput{
		DhcpOptionSetID: id,
		Name:            cwutil.GetPtr(fromSite.Name),
		Description:     cwutil.GetPtr(coreDOS.GetMetadata().GetDescription()),
		Options:         fromSite.Options,
		Labels:          labels,
		Version:         cwutil.GetPtr(coreDOS.GetVersion()),
	}
}

// updateDhcpOptionSetStatusInDB updates the status and records it in the status history
func (mdos ManageDhcpOptionSet) updateDhcpOptionSetStatusInDB(ctx context.Context, tx *cdb.Tx, id uuid.UUID, status cdbm.DhcpOptionSetStatus, message string) error {
	_, err := cdbm.NewDhcpOptionSetDAO(mdos.dbSession).Update(ctx, tx, cdbm.DhcpOptionSetUpdateInput{
		DhcpOptionSetID: id,
		Status:          &status,
	})
	if err != nil {
		return err
	}

	_, err = cdbm.NewStatusDetailDAO(mdos.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
		EntityID: id.String(),
		Status:   string(status),
		Message:  &message,
	})
	return err
}

// NewManageDhcpOptionSet returns a new ManageDhcpOptionSet activity
func NewManageDhcpOptionSet(dbSession *cdb.Session) ManageDhcpOptionSet {
	return ManageDhcpOptionSet{
		dbSession: dbSession,
	}
}
