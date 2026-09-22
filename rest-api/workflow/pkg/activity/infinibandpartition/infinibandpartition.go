// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package infinibandpartition

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

// ManageInfiniBandPartition is an activity wrapper for managing InfiniBandPartition lifecycle that allows
// injecting DB access
type ManageInfiniBandPartition struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions
// UpdateInfiniBandPartitionsInDB is a Temporal activity that takes a collection of InfiniBandPartition data pushed by Site Agent and updates the DB
func (mibp ManageInfiniBandPartition) UpdateInfiniBandPartitionsInDB(ctx context.Context, siteID uuid.UUID, ibpInventory *corev1.InfiniBandPartitionInventory) error {
	logger := log.With().Str("Activity", "UpdateInfiniBandPartitionsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	stDAO := cdbm.NewSiteDAO(mibp.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received InfiniBand Partition inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	if ibpInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	ibpDAO := cdbm.NewInfiniBandPartitionDAO(mibp.dbSession)

	existingIbps, _, err := ibpDAO.GetAll(
		ctx,
		nil,
		cdbm.InfiniBandPartitionFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		},
		cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get InfiniBand Partition for Site from DB")
		return err
	}

	// Construct a map of Controller InfiniBandPartition ID to InfiniBandPartition
	existingIbpIDMap := make(map[string]*cdbm.InfiniBandPartition)

	for _, ibp := range existingIbps {
		curIbp := ibp
		existingIbpIDMap[ibp.ID.String()] = &curIbp
		if ibp.ControllerIBPartitionID != nil {
			existingIbpIDMap[ibp.ControllerIBPartitionID.String()] = &curIbp
		}
	}

	reportedIbpIDMap := map[uuid.UUID]bool{}

	if ibpInventory.InventoryPage != nil {
		logger.Info().Msgf("Received InfiniBand Partition inventory page: %d of %d, page size: %d, total count: %d",
			ibpInventory.InventoryPage.CurrentPage, ibpInventory.InventoryPage.TotalPages,
			ibpInventory.InventoryPage.PageSize, ibpInventory.InventoryPage.TotalItems)

		for _, strId := range ibpInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strId).Msg("failed to parse InfiniBand Partition ID from inventory page")
				continue
			}
			reportedIbpIDMap[id] = true
		}
	}

	// Iterate through InfiniBandPartition Inventory and update DB
	for _, controllerIbp := range ibpInventory.IbPartitions {
		slogger := logger.With().Str("InfiniBand Partition Controller ID", controllerIbp.Id.Value).Logger()

		// TODO: Since Site is the source of truth, we must auto-create any Partitions that are in the Site inventory but not in the DB
		ibp, ok := existingIbpIDMap[controllerIbp.Id.Value]
		if !ok && controllerIbp.Config != nil {
			ibp, ok = existingIbpIDMap[controllerIbp.Config.Name]
		}

		if !ok {
			slogger.Error().Str("Controller IB Partition ID", controllerIbp.Id.Value).Msg("InfiniBand Partition does not have a record in DB, possibly created directly on Site")
			continue
		}

		reportedIbpIDMap[ibp.ID] = true

		isUpdateRequired := false
		// Reset missing flag if necessary
		var isMissingOnSite *bool
		if ibp.IsMissingOnSite {
			isMissingOnSite = cwutil.GetPtr(false)
			isUpdateRequired = true
		}

		// Populate controller InfiniBandPartition ID if necessary
		var controllerIbpID *uuid.UUID
		if ibp.ControllerIBPartitionID == nil {
			ctrlID, serr := uuid.Parse(controllerIbp.Id.Value)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to parse InfiniBand Partition Controller ID, not a valid UUID")
				continue
			}
			controllerIbpID = &ctrlID
			isUpdateRequired = true
		}

		// Populate InfiniBandPartition info from status
		var partitionKey, partitionName *string
		var serviceLevel, mtu *int
		var rateLimit *float32
		var enableSharp *bool

		if controllerIbp.Status != nil {
			if controllerIbp.Status.Pkey != nil {
				partitionKey = controllerIbp.Status.Pkey
				isUpdateRequired = true
			}

			if controllerIbp.Status.Partition != nil {
				partitionName = controllerIbp.Status.Partition
				isUpdateRequired = true
			}

			if controllerIbp.Status.ServiceLevel != nil {
				val := int(*controllerIbp.Status.ServiceLevel)
				serviceLevel = &val
				isUpdateRequired = true
			}

			if controllerIbp.Status.RateLimit != nil {
				val := float32(*controllerIbp.Status.RateLimit)
				rateLimit = &val
				isUpdateRequired = true
			}

			if controllerIbp.Status.Mtu != nil {
				val := int(*controllerIbp.Status.Mtu)
				mtu = &val
				isUpdateRequired = true
			}

			if controllerIbp.Status.EnableSharp != nil {
				enableSharp = controllerIbp.Status.EnableSharp
				isUpdateRequired = true
			}
		}

		if isUpdateRequired {
			_, serr := ibpDAO.Update(
				ctx,
				nil,
				cdbm.InfiniBandPartitionUpdateInput{
					InfiniBandPartitionID:   ibp.ID,
					ControllerIBPartitionID: controllerIbpID,
					PartitionKey:            partitionKey,
					PartitionName:           partitionName,
					ServiceLevel:            serviceLevel,
					RateLimit:               rateLimit,
					Mtu:                     mtu,
					EnableSharp:             enableSharp,
					IsMissingOnSite:         isMissingOnSite,
				},
			)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update InfiniBand Partition data in DB")
				continue
			}
		}

		// Update status if necessary
		if controllerIbp.Status != nil {
			if ibp.Status == cdbm.InfiniBandPartitionStatusDeleting {
				continue
			}

			var status cdbm.InfiniBandPartitionStatus
			status.FromProto(controllerIbp.Status.State)

			if status != "" && status != ibp.Status {
				message := status.Message()
				err = mibp.updateIBPStatusInDB(ctx, nil, ibp.ID, &status, &message)
				if err != nil {
					slogger.Error().Err(err).Msg("failed to update InfiniBand Partition status detail in DB")
				}
			}
		}

	}

	// Populate list of ibps that were not found
	ibpsToDelete := []*cdbm.InfiniBandPartition{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if ibpInventory.InventoryPage == nil || ibpInventory.InventoryPage.TotalPages == 0 || (ibpInventory.InventoryPage.CurrentPage == ibpInventory.InventoryPage.TotalPages) {
		for _, ibp := range existingIbpIDMap {
			found := false

			_, found = reportedIbpIDMap[ibp.ID]
			if !found && ibp.ControllerIBPartitionID != nil {
				// Additional check if controller IBPartition ID != Instance ID
				_, found = reportedIbpIDMap[*ibp.ControllerIBPartitionID]
			}

			if !found {
				// The InfiniBandPartition was not found in the InfiniBandPartition Inventory, so add it to list of InfiniBandPartition to potentially delete
				ibpsToDelete = append(ibpsToDelete, ibp)
			}
		}
	}

	// Loop through ibps for deletion
	for _, ibp := range ibpsToDelete {
		slogger := logger.With().Str("Partition ID", ibp.ID.String()).Logger()

		// If the InfiniBandPartition was already being deleted, we can proceed with removing it from the DB
		if ibp.Status == cdbm.InfiniBandPartitionStatusDeleting {
			serr := ibpDAO.Delete(ctx, nil, ibp.ID)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to delete InfiniBand Partition from DB")
			}
		} else if ibp.ControllerIBPartitionID != nil {
			// Was this created within inventory receipt interval? If so, we may be processing an older inventory
			if site.IsTimeWithinStaleInventoryThreshold(ibp.Created) {
				continue
			}

			// Set isMissingOnSite flag to true and update status, user can decide on deletion
			_, serr := ibpDAO.Update(
				ctx,
				nil,
				cdbm.InfiniBandPartitionUpdateInput{
					InfiniBandPartitionID: ibp.ID,
					IsMissingOnSite:       cwutil.GetPtr(true),
				},
			)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to set missing on Site flag in DB for InfiniBand Partition")
				continue
			}

			errStatus := cdbm.InfiniBandPartitionStatusError
			serr = mibp.updateIBPStatusInDB(ctx, nil, ibp.ID, &errStatus, cwutil.GetPtr("InfiniBand Partition is missing on Site"))
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update InfiniBand Partition status detail in DB")
			}
		}
	}

	return nil
}

// updateIBPStatusInDB is helper function to write InfiniBandPartition updates to DB
func (mibp ManageInfiniBandPartition) updateIBPStatusInDB(ctx context.Context, tx *cdb.Tx, ibpID uuid.UUID, status *cdbm.InfiniBandPartitionStatus, statusMessage *string) error {
	if status != nil {
		ibpDAO := cdbm.NewInfiniBandPartitionDAO(mibp.dbSession)

		_, err := ibpDAO.Update(
			ctx,
			tx,
			cdbm.InfiniBandPartitionUpdateInput{
				InfiniBandPartitionID: ibpID,
				Status:                status,
			},
		)
		if err != nil {
			return err
		}

		statusDetailDAO := cdbm.NewStatusDetailDAO(mibp.dbSession)
		_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: ibpID.String(), Status: string(*status), Message: statusMessage})
		if err != nil {
			return err
		}
	}
	return nil
}

// NewManageInfiniBandPartition returns a new ManageInfiniBandPartition activity
func NewManageInfiniBandPartition(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageInfiniBandPartition {
	return ManageInfiniBandPartition{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
