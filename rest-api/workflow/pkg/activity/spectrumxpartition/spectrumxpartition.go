// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package spectrumxpartition

import (
	"context"
	"fmt"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageSpectrumXPartition is an activity wrapper for managing SpectrumXPartition lifecycle that
// allows injecting DB access
type ManageSpectrumXPartition struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// UpdateSpectrumXPartitionsInDB is a Temporal activity that takes a collection of
// SpectrumXPartition data pushed by Site Agent and updates the DB.
//
// forge.SpxPartition carries no status sub-message, unlike IBPartition, so presence in the
// inventory is the only signal the Site gives. A Partition the Site reports is promoted to
// Ready; one that stops being reported is either removed (when already Deleting) or flagged
// as missing.
func (msxp ManageSpectrumXPartition) UpdateSpectrumXPartitionsInDB(ctx context.Context, siteID uuid.UUID, sxpInventory *corev1.SpectrumXPartitionInventory) error {
	logger := log.With().Str("Activity", "UpdateSpectrumXPartitionsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	stDAO := cdbm.NewSiteDAO(msxp.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received SpectrumX Partition inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	if sxpInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	sxpDAO := cdbm.NewSpectrumXPartitionDAO(msxp.dbSession)

	existingSxps, _, err := sxpDAO.GetAll(
		ctx,
		nil,
		cdbm.SpectrumXPartitionFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		},
		cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get SpectrumX Partition for Site from DB")
		return err
	}

	// Core creates each Partition under the ID this side supplied, so the REST ID is
	// also the ID the Site reports back.
	existingSxpIDMap := make(map[string]*cdbm.SpectrumXPartition, len(existingSxps))

	for _, sxp := range existingSxps {
		curSxp := sxp
		existingSxpIDMap[sxp.ID.String()] = &curSxp
	}

	reportedSxpIDMap := map[uuid.UUID]bool{}

	if sxpInventory.InventoryPage != nil {
		logger.Info().Msgf("Received SpectrumX Partition inventory page: %d of %d, page size: %d, total count: %d",
			sxpInventory.InventoryPage.CurrentPage, sxpInventory.InventoryPage.TotalPages,
			sxpInventory.InventoryPage.PageSize, sxpInventory.InventoryPage.TotalItems)

		for _, strID := range sxpInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strID)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strID).Msg("failed to parse SpectrumX Partition ID from inventory page")
				continue
			}
			reportedSxpIDMap[id] = true
		}
	}

	// Iterate through SpectrumXPartition Inventory and update DB
	for _, controllerSxp := range sxpInventory.SpxPartitions {
		slogger := logger.With().Str("SpectrumX Partition ID", controllerSxp.GetId().GetValue()).Logger()

		sxp, ok := existingSxpIDMap[controllerSxp.GetId().GetValue()]

		// No active REST row for this inventory Partition: create one or undelete a soft-deleted
		// match, then fall through so the main loop applies Site-reported field updates.
		if !ok {
			sxp = msxp.createOrUpdateSpectrumXPartitionFromSite(ctx, site, controllerSxp)
			if sxp == nil {
				continue
			}

			// Keep the in-memory map in sync so later inventory entries see this Partition.
			existingSxpIDMap[sxp.ID.String()] = sxp
			slogger.Info().Str("SpectrumX Partition ID", sxp.ID.String()).Msg("created or undeleted SpectrumX Partition from Site inventory")
		}

		reportedSxpIDMap[sxp.ID] = true

		isUpdateRequired := false

		// Reset missing flag if necessary
		var isMissingOnSite *bool
		if sxp.IsMissingOnSite {
			isMissingOnSite = cwutil.GetPtr(false)
			isUpdateRequired = true
		}

		// The Site owns VNI allocation, so take its value whenever it differs.
		var vni *int
		if controllerSxp.GetVni() != 0 {
			reported := int(controllerSxp.GetVni())
			if sxp.VNI == nil || *sxp.VNI != reported {
				vni = &reported
				isUpdateRequired = true
			}
		}

		if isUpdateRequired {
			_, serr := sxpDAO.Update(
				ctx,
				nil,
				cdbm.SpectrumXPartitionUpdateInput{
					SpectrumXPartitionID: sxp.ID,
					VNI:                  vni,
					IsMissingOnSite:      isMissingOnSite,
				},
			)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update SpectrumX Partition data in DB")
				continue
			}
		}

		// A Partition on its way out keeps its Deleting status until the Site stops
		// reporting it, which the deletion sweep below acts on.
		if sxp.Status == cdbm.SpectrumXPartitionStatusDeleting {
			continue
		}

		// Being reported by the Site is what makes a Partition Ready. The status row and its
		// status detail move together so a failure cannot leave the two disagreeing.
		if sxp.Status != cdbm.SpectrumXPartitionStatusReady {
			readyStatus := cdbm.SpectrumXPartitionStatusReady
			message := readyStatus.Message()
			serr := cdb.WithTx(ctx, msxp.dbSession, func(tx *cdb.Tx) error {
				return msxp.updateSpectrumXPartitionStatusInDB(ctx, tx, sxp.ID, &readyStatus, &message)
			})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update SpectrumX Partition status detail in DB")
			}
		}
	}

	// Populate list of Partitions that were not found
	sxpsToDelete := []*cdbm.SpectrumXPartition{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if sxpInventory.InventoryPage == nil || sxpInventory.InventoryPage.TotalPages == 0 || (sxpInventory.InventoryPage.CurrentPage == sxpInventory.InventoryPage.TotalPages) {
		for i := range existingSxps {
			sxp := &existingSxps[i]

			if !reportedSxpIDMap[sxp.ID] {
				// The SpectrumXPartition was not found in the inventory, so add it to the list to potentially delete
				sxpsToDelete = append(sxpsToDelete, sxp)
			}
		}
	}

	// Loop through Partitions for deletion
	for _, sxp := range sxpsToDelete {
		slogger := logger.With().Str("Partition ID", sxp.ID.String()).Logger()

		// If the SpectrumXPartition was already being deleted, we can proceed with removing it from the DB
		if sxp.Status == cdbm.SpectrumXPartitionStatusDeleting {
			serr := sxpDAO.Delete(ctx, nil, sxp.ID)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to delete SpectrumX Partition from DB")
			}
			continue
		}

		// Was this created within inventory receipt interval? If so, we may be processing an older inventory
		if site.IsTimeWithinStaleInventoryThreshold(sxp.Created) {
			continue
		}

		// Already recorded as missing, so re-applying it would only append a duplicate
		// status detail on every inventory cycle.
		if sxp.IsMissingOnSite && sxp.Status == cdbm.SpectrumXPartitionStatusError {
			continue
		}

		// Set isMissingOnSite flag to true and update status, user can decide on deletion.
		// The flag, the status and the status detail move together so a failure part way
		// through cannot leave the Partition state and its history disagreeing.
		errStatus := cdbm.SpectrumXPartitionStatusError
		serr := cdb.WithTx(ctx, msxp.dbSession, func(tx *cdb.Tx) error {
			if _, derr := sxpDAO.Update(
				ctx,
				tx,
				cdbm.SpectrumXPartitionUpdateInput{
					SpectrumXPartitionID: sxp.ID,
					IsMissingOnSite:      cwutil.GetPtr(true),
				},
			); derr != nil {
				return derr
			}

			return msxp.updateSpectrumXPartitionStatusInDB(ctx, tx, sxp.ID, &errStatus, cwutil.GetPtr("SpectrumX Partition is missing on Site"))
		})
		if serr != nil {
			slogger.Error().Err(serr).Msg("failed to record SpectrumX Partition as missing on Site")
		}
	}

	return nil
}

// createOrUpdateSpectrumXPartitionFromSite creates a REST SpectrumX Partition from Site
// inventory, or undeletes a matching soft-deleted row. Field refresh after undelete is left
// to UpdateSpectrumXPartitionsInDB. Returns nil when skipped or on failure.
func (msxp ManageSpectrumXPartition) createOrUpdateSpectrumXPartitionFromSite(
	ctx context.Context,
	site *cdbm.Site,
	controllerSxp *corev1.SpxPartition,
) *cdbm.SpectrumXPartition {
	logger := log.With().
		Str("Activity", "UpdateSpectrumXPartitionsInDB").
		Str("Site ID", site.ID.String()).
		Str("SpectrumX Partition ID", controllerSxp.GetId().GetValue()).
		Logger()

	sxpID, err := uuid.Parse(controllerSxp.GetId().GetValue())
	if err != nil {
		logger.Warn().Msgf("unable to create SpectrumX Partition found on Site: failed to parse ID, not a valid UUID %s", controllerSxp.GetId().GetValue())
		return nil
	}

	org := controllerSxp.GetTenantOrganizationId()
	if org == "" {
		logger.Warn().Msg("unable to create SpectrumX Partition found on Site: Partition on Site is reporting empty Tenant organization ID")
		return nil
	}

	name := controllerSxp.GetMetadata().GetName()
	if name == "" {
		name = fmt.Sprintf("recovered-%s", sxpID.String()[:8])
	}

	var description *string
	desc := controllerSxp.GetMetadata().GetDescription()
	if desc != "" {
		description = &desc
	}

	var labels cdbm.Labels
	labels.FromProto(controllerSxp.GetMetadata().GetLabels())

	var vni *int
	if controllerSxp.GetVni() != 0 {
		vni = cwutil.GetPtr(int(controllerSxp.GetVni()))
	}

	readyStatus := cdbm.SpectrumXPartitionStatusReady
	readyMsg := "SpectrumX Partition was found on Site, Ready for use"

	// Create/undelete under one transaction so concurrent inventory pages cannot insert duplicates.
	sxp, err := cdb.WithTxResult(ctx, msxp.dbSession, func(tx *cdb.Tx) (*cdbm.SpectrumXPartition, error) {
		sxpDAO := cdbm.NewSpectrumXPartitionDAO(msxp.dbSession)

		// Core creates each Partition under the ID this side supplied, so primary-key lookup is sufficient.
		matches, _, reloadErr := sxpDAO.GetAll(ctx, tx, cdbm.SpectrumXPartitionFilterInput{
			SpectrumXPartitionIDs: []uuid.UUID{sxpID}, SiteIDs: []uuid.UUID{site.ID}, IncludeDeleted: true,
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if reloadErr != nil {
			return nil, fmt.Errorf("unable to create SpectrumX Partition found on Site: failed to retrieve Partition by ID, DB error: %w", reloadErr)
		}

		if len(matches) > 0 {
			existingSxp := &matches[0]
			if existingSxp.Deleted == nil {
				return existingSxp, nil
			}
			if existingSxp.Org != org {
				logger.Warn().Msgf("unable to create SpectrumX Partition found on Site: tenant organization differs in REST cache and Site record %s", org)
				return nil, nil
			}
			// Deleted records when the delete happened, so a delete newer than the interval can
			// postdate this inventory. Undeleting then would revive a Partition the snapshot never
			// saw removed. A later inventory undeletes it if the Site still reports it.
			if site.IsTimeWithinStaleInventoryThreshold(*existingSxp.Deleted) {
				logger.Info().Msgf("not undeleting SpectrumX Partition %s yet because it was deleted more recently than the inventory interval", sxpID)
				return nil, nil
			}

			restored, clearErr := sxpDAO.Clear(ctx, tx, cdbm.SpectrumXPartitionClearInput{SpectrumXPartitionID: existingSxp.ID, Deleted: true})
			if clearErr != nil {
				return nil, fmt.Errorf("unable to create SpectrumX Partition found on Site: failed to clear soft-delete timestamp for Partition, DB error: %w", clearErr)
			}

			// A row only reaches soft-deletion from Deleting, and UpdateSpectrumXPartitionsInDB
			// never promotes a Deleting row, so the undelete has to restore Ready itself or the
			// Partition would sit in Deleting for as long as the Site keeps reporting it.
			// Other Site-reported field updates are left to UpdateSpectrumXPartitionsInDB.
			if restored.Status != readyStatus {
				statusErr := msxp.updateSpectrumXPartitionStatusInDB(ctx, tx, restored.ID, &readyStatus, &readyMsg)
				if statusErr != nil {
					return nil, fmt.Errorf("unable to create SpectrumX Partition found on Site: failed to restore status for undeleted Partition, DB error: %w", statusErr)
				}
				restored.Status = readyStatus
			}
			return restored, nil
		}

		tenants, _, tenantErr := cdbm.NewTenantDAO(msxp.dbSession).GetAll(
			ctx, tx, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil,
		)
		if tenantErr != nil {
			return nil, fmt.Errorf("unable to create SpectrumX Partition found on Site: failed to retrieve Tenant by organization, DB error: %w", tenantErr)
		}
		if len(tenants) == 0 {
			logger.Warn().Msgf("unable to create SpectrumX Partition found on Site: no Tenants were found for org: %s", org)
			return nil, nil
		}
		tenant := &tenants[0]

		// If an active Partition already uses this name for the Tenant/Site, append a recovered suffix.
		nameConflicts, _, nameErr := sxpDAO.GetAll(ctx, tx, cdbm.SpectrumXPartitionFilterInput{
			Names: []string{name}, TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{site.ID},
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if nameErr != nil {
			return nil, fmt.Errorf("unable to create SpectrumX Partition found on Site: failed to retrieve Partition by name, DB error: %w", nameErr)
		}
		if len(nameConflicts) > 0 {
			name = fmt.Sprintf("%s-recovered-%s", name, sxpID.String()[:8])
		}

		created, createErr := sxpDAO.Create(ctx, tx, cdbm.SpectrumXPartitionCreateInput{
			SpectrumXPartitionID: &sxpID,
			Name:                 name,
			Description:          description,
			TenantOrg:            org,
			SiteID:               site.ID,
			TenantID:             tenant.ID,
			VNI:                  vni,
			Labels:               labels,
			Status:               readyStatus,
			CreatedBy:            tenant.CreatedBy,
		})
		if createErr != nil {
			return nil, fmt.Errorf("unable to create SpectrumX Partition found on Site: failed to create Partition, DB error: %w", createErr)
		}

		_, statusErr := cdbm.NewStatusDetailDAO(msxp.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: created.ID.String(), Status: string(readyStatus), Message: &readyMsg,
		})
		if statusErr != nil {
			return nil, fmt.Errorf("unable to create SpectrumX Partition found on Site: failed to create Status Detail, DB error: %w", statusErr)
		}
		return created, nil
	})
	if err != nil {
		logger.Warn().Err(err).Msg("failed to create or undelete SpectrumX Partition from Site inventory")
		return nil
	}
	return sxp
}

// updateSpectrumXPartitionStatusInDB is a helper function to write SpectrumXPartition status updates to DB
func (msxp ManageSpectrumXPartition) updateSpectrumXPartitionStatusInDB(ctx context.Context, tx *cdb.Tx, sxpID uuid.UUID, status *cdbm.SpectrumXPartitionStatus, statusMessage *string) error {
	if status == nil {
		return nil
	}

	sxpDAO := cdbm.NewSpectrumXPartitionDAO(msxp.dbSession)

	_, err := sxpDAO.Update(
		ctx,
		tx,
		cdbm.SpectrumXPartitionUpdateInput{
			SpectrumXPartitionID: sxpID,
			Status:               status,
		},
	)
	if err != nil {
		return err
	}

	statusDetailDAO := cdbm.NewStatusDetailDAO(msxp.dbSession)
	_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: sxpID.String(), Status: string(*status), Message: statusMessage})
	if err != nil {
		return err
	}

	return nil
}

// NewManageSpectrumXPartition returns a new ManageSpectrumXPartition activity
func NewManageSpectrumXPartition(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageSpectrumXPartition {
	return ManageSpectrumXPartition{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
