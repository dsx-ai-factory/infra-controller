// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package spectrumxpartition

import (
	"context"

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

		// TODO: Since Site is the source of truth, we must auto-create any Partitions that are in the Site inventory but not in the DB
		sxp, ok := existingSxpIDMap[controllerSxp.GetId().GetValue()]
		if !ok {
			slogger.Error().Msg("SpectrumX Partition does not have a record in DB, possibly created directly on Site")
			continue
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

		// Being reported by the Site is what makes a Partition Ready.
		if sxp.Status != cdbm.SpectrumXPartitionStatusReady {
			readyStatus := cdbm.SpectrumXPartitionStatusReady
			message := readyStatus.Message()
			serr := msxp.updateSpectrumXPartitionStatusInDB(ctx, nil, sxp.ID, &readyStatus, &message)
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

		// Set isMissingOnSite flag to true and update status, user can decide on deletion
		_, serr := sxpDAO.Update(
			ctx,
			nil,
			cdbm.SpectrumXPartitionUpdateInput{
				SpectrumXPartitionID: sxp.ID,
				IsMissingOnSite:      cwutil.GetPtr(true),
			},
		)
		if serr != nil {
			slogger.Error().Err(serr).Msg("failed to set missing on Site flag in DB for SpectrumX Partition")
			continue
		}

		errStatus := cdbm.SpectrumXPartitionStatusError
		serr = msxp.updateSpectrumXPartitionStatusInDB(ctx, nil, sxp.ID, &errStatus, cwutil.GetPtr("SpectrumX Partition is missing on Site"))
		if serr != nil {
			slogger.Error().Err(serr).Msg("failed to update SpectrumX Partition status detail in DB")
		}
	}

	return nil
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
