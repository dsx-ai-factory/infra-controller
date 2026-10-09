// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package infinibandpartition

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

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

	// Construct a map of InfiniBand Partition ID to InfiniBand Partition.
	existingIbpIDMap := make(map[string]*cdbm.InfiniBandPartition)

	for _, ibp := range existingIbps {
		curIbp := ibp
		existingIbpIDMap[ibp.ID.String()] = &curIbp
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

		if !ok {
			ibp = mibp.createOrUpdateInfiniBandPartitionFromSite(ctx, site, controllerIbp)
			if ibp == nil {
				continue
			}

			existingIbpIDMap[ibp.ID.String()] = ibp

			slogger.Info().Str("InfiniBand Partition ID", ibp.ID.String()).Msg("created or undeleted InfiniBand Partition from Site inventory")
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
	if util.ShouldReconcileDeletions(ibpInventory.GetInventoryPage()) {
		for _, ibp := range existingIbpIDMap {
			_, found := reportedIbpIDMap[ibp.ID]

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

// createOrUpdateInfiniBandPartitionFromSite creates a REST InfiniBand Partition from Site
// inventory, or undeletes a matching soft-deleted row. Returns nil when skipped or on failure.
//
//nolint:cyclop,exhaustruct,funlen,gocognit,gocyclo,maintidx,nestif,nilnil,varnamelen // Recovery is one transactional flow, matching VPC Prefix recovery.
func (mibp ManageInfiniBandPartition) createOrUpdateInfiniBandPartitionFromSite(
	ctx context.Context,
	site *cdbm.Site,
	controllerIbp *corev1.IBPartition,
) *cdbm.InfiniBandPartition {
	logger := log.With().
		Str("Activity", "UpdateInfiniBandPartitionsInDB").
		Str("Site ID", site.ID.String()).
		Str("InfiniBand Partition Controller ID", controllerIbp.GetId().GetValue()).
		Logger()

	controllerIbpID, err := uuid.Parse(controllerIbp.GetId().GetValue())
	if err != nil {
		logger.Warn().Msgf("unable to create InfiniBand Partition found on Site: failed to parse ID, not a valid UUID %s", controllerIbp.GetId().GetValue())

		return nil
	}

	reportedIbp := new(cdbm.InfiniBandPartition)
	reportedIbp.FromProto(controllerIbp)

	if reportedIbp.Org == "" {
		logger.Warn().Msg("unable to create InfiniBand Partition found on Site: Partition is reporting empty Tenant organization ID")

		return nil
	}

	if reportedIbp.Name == "" {
		reportedIbp.Name = "recovered-" + controllerIbpID.String()[:8]
	}

	status := cdbm.InfiniBandPartitionStatusReady

	var (
		partitionKey, partitionName *string
		serviceLevel, mtu           *int
		rateLimit                   *float32
		enableSharp                 *bool
	)

	if controllerIbp.GetStatus() != nil {
		reportedState := controllerIbp.GetStatus().GetState()
		if reportedState == corev1.TenantState_TERMINATING || reportedState == corev1.TenantState_TERMINATED {
			logger.Info().Msgf("skipping create or undelete of InfiniBand Partition from Site inventory: Site reports state %s", reportedState)

			return nil
		}

		reportedStatus := cdbm.InfiniBandPartitionStatus("")
		reportedStatus.FromProto(reportedState)

		if reportedStatus == "" {
			logger.Warn().Msgf("unable to create InfiniBand Partition found on Site: unsupported state %s", reportedState)

			return nil
		}

		status = reportedStatus

		partitionKey = controllerIbp.GetStatus().Pkey
		partitionName = controllerIbp.GetStatus().Partition

		if controllerIbp.GetStatus().ServiceLevel != nil {
			serviceLevel = cwutil.GetPtr(int(controllerIbp.GetStatus().GetServiceLevel()))
		}

		if controllerIbp.GetStatus().RateLimit != nil {
			rateLimit = cwutil.GetPtr(float32(controllerIbp.GetStatus().GetRateLimit()))
		}

		if controllerIbp.GetStatus().Mtu != nil {
			mtu = cwutil.GetPtr(int(controllerIbp.GetStatus().GetMtu()))
		}

		enableSharp = controllerIbp.GetStatus().EnableSharp
	}

	if partitionKey == nil && controllerIbp.GetConfig() != nil {
		partitionKey = controllerIbp.GetConfig().Pkey
	}

	statusMessage := "InfiniBand Partition was found on Site"
	if status == cdbm.InfiniBandPartitionStatusReady {
		statusMessage += ", ready for use"
	}

	ibp, err := cdb.WithTxResult(ctx, mibp.dbSession, func(tx *cdb.Tx) (*cdbm.InfiniBandPartition, error) {
		ibpDAO := cdbm.NewInfiniBandPartitionDAO(mibp.dbSession)

		tenants, _, tenantErr := cdbm.NewTenantDAO(mibp.dbSession).GetAll(
			ctx,
			tx,
			cdbm.TenantFilterInput{Orgs: []string{reportedIbp.Org}},
			cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if tenantErr != nil {
			return nil, fmt.Errorf("unable to create InfiniBand Partition found on Site: failed to retrieve Tenant by organization, DB error: %w", tenantErr)
		}

		if len(tenants) == 0 {
			logger.Warn().Msgf("unable to create InfiniBand Partition found on Site: no Tenants were found for org: %s", reportedIbp.Org)

			return nil, nil
		}

		tenant := &tenants[0]

		_, tenantSiteErr := cdbm.NewTenantSiteDAO(mibp.dbSession).GetByTenantIDAndSiteID(ctx, tx, tenant.ID, site.ID, nil)
		if tenantSiteErr != nil {
			if errors.Is(tenantSiteErr, cdb.ErrDoesNotExist) {
				logger.Warn().Msgf("unable to create InfiniBand Partition found on Site: Tenant for org %s does not have access to Site", reportedIbp.Org)

				return nil, nil
			}

			return nil, fmt.Errorf("unable to create InfiniBand Partition found on Site: failed to validate Tenant access to Site, DB error: %w", tenantSiteErr)
		}

		lockErr := tx.TryAcquireAdvisoryLock(
			ctx,
			cdb.GetAdvisoryLockIDFromString(
				fmt.Sprintf("infiniband-partition-recovery-%s-%s", tenant.ID, site.ID),
			),
			nil,
		)
		if lockErr != nil {
			return nil, fmt.Errorf("unable to create InfiniBand Partition found on Site: failed to acquire recovery lock, DB error: %w", lockErr)
		}

		matches, _, reloadErr := ibpDAO.GetAll(
			ctx,
			tx,
			cdbm.InfiniBandPartitionFilterInput{
				InfiniBandPartitionIDs: []uuid.UUID{controllerIbpID},
				IncludeDeleted:         true,
			},
			cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if reloadErr != nil {
			return nil, fmt.Errorf("unable to create InfiniBand Partition found on Site: failed to retrieve Partition by ID, DB error: %w", reloadErr)
		}

		var existingIbp *cdbm.InfiniBandPartition
		if len(matches) > 0 {
			existingIbp = &matches[0]
			if existingIbp.SiteID != site.ID {
				logger.Warn().Msg("unable to create InfiniBand Partition found on Site: Partition ID belongs to a different Site in REST cache")

				return nil, nil
			}

			if existingIbp.TenantID != tenant.ID || existingIbp.Org != reportedIbp.Org {
				logger.Warn().Msgf("unable to create InfiniBand Partition found on Site: tenant organization differs in REST cache and Site record %s", reportedIbp.Org)

				return nil, nil
			}

			if existingIbp.Deleted == nil {
				return existingIbp, nil
			}

			if site.IsTimeWithinStaleInventoryThreshold(*existingIbp.Deleted) {
				logger.Info().Msgf("not undeleting InfiniBand Partition %s yet because it was deleted more recently than the inventory interval", controllerIbpID)

				return nil, nil
			}

			if reportedIbp.Name == existingIbp.ID.String() {
				reportedIbp.Name = existingIbp.Name
			}
		}

		baseName := reportedIbp.Name
		baseNameRunes := []rune(baseName)

		if len(baseNameRunes) < cdbm.InfiniBandPartitionNameMinLength ||
			len(baseNameRunes) > cdbm.InfiniBandPartitionNameMaxLength ||
			strings.TrimSpace(baseName) != baseName {
			baseName = "recovered-" + controllerIbpID.String()[:8]
		}

		reportedIbp.Name = baseName
		for attempt := 1; ; attempt++ {
			nameConflicts, _, nameErr := ibpDAO.GetAll(
				ctx,
				tx,
				cdbm.InfiniBandPartitionFilterInput{
					Names:     []string{reportedIbp.Name},
					SiteIDs:   []uuid.UUID{site.ID},
					TenantIDs: []uuid.UUID{tenant.ID},
				},
				cdbp.PageInput{Limit: cwutil.GetPtr(1)},
				nil,
			)
			if nameErr != nil {
				return nil, fmt.Errorf("unable to create InfiniBand Partition found on Site: failed to retrieve Partition by name, DB error: %w", nameErr)
			}

			if len(nameConflicts) == 0 {
				break
			}

			suffix := "-recovered-" + controllerIbpID.String()[:8]
			if attempt > 1 {
				suffix = fmt.Sprintf("%s-%d", suffix, attempt)
			}

			maxBaseRunes := cdbm.InfiniBandPartitionNameMaxLength - len([]rune(suffix))
			nameRunes := []rune(baseName)

			if len(nameRunes) > maxBaseRunes {
				nameRunes = nameRunes[:maxBaseRunes]
			}

			reportedIbp.Name = string(nameRunes) + suffix
		}

		if existingIbp != nil {
			restored, clearErr := ibpDAO.Clear(ctx, tx, cdbm.InfiniBandPartitionClearInput{
				InfiniBandPartitionID: existingIbp.ID,
				Description:           reportedIbp.Description == nil,
				PartitionKey:          partitionKey == nil,
				PartitionName:         partitionName == nil,
				ServiceLevel:          serviceLevel == nil,
				RateLimit:             rateLimit == nil,
				Mtu:                   mtu == nil,
				EnableSharp:           enableSharp == nil,
				Labels:                reportedIbp.Labels == nil,
				Deleted:               true,
			})
			if clearErr != nil {
				return nil, fmt.Errorf("unable to create InfiniBand Partition found on Site: failed to clear soft-delete timestamp, DB error: %w", clearErr)
			}

			restored, updateErr := ibpDAO.Update(ctx, tx, cdbm.InfiniBandPartitionUpdateInput{
				InfiniBandPartitionID:   restored.ID,
				Name:                    &reportedIbp.Name,
				Description:             reportedIbp.Description,
				ControllerIBPartitionID: &controllerIbpID,
				PartitionKey:            partitionKey,
				PartitionName:           partitionName,
				ServiceLevel:            serviceLevel,
				RateLimit:               rateLimit,
				Mtu:                     mtu,
				EnableSharp:             enableSharp,
				Labels:                  map[string]string(reportedIbp.Labels),
				Status:                  &status,
				IsMissingOnSite:         cwutil.GetPtr(false),
			})
			if updateErr != nil {
				return nil, fmt.Errorf("unable to create InfiniBand Partition found on Site: failed to restore Partition data, DB error: %w", updateErr)
			}

			_, statusErr := cdbm.NewStatusDetailDAO(mibp.dbSession).Create(
				ctx,
				tx,
				cdbm.StatusDetailCreateInput{
					EntityID: restored.ID.String(),
					Status:   string(status),
					Message:  &statusMessage,
				},
			)
			if statusErr != nil {
				return nil, fmt.Errorf("unable to create InfiniBand Partition found on Site: failed to create Status Detail after undelete, DB error: %w", statusErr)
			}

			return restored, nil
		}

		created, createErr := ibpDAO.Create(ctx, tx, cdbm.InfiniBandPartitionCreateInput{
			InfiniBandPartitionID:   &controllerIbpID,
			Name:                    reportedIbp.Name,
			Description:             reportedIbp.Description,
			TenantOrg:               reportedIbp.Org,
			SiteID:                  site.ID,
			TenantID:                tenant.ID,
			ControllerIBPartitionID: &controllerIbpID,
			PartitionKey:            partitionKey,
			PartitionName:           partitionName,
			ServiceLevel:            serviceLevel,
			RateLimit:               rateLimit,
			Mtu:                     mtu,
			EnableSharp:             enableSharp,
			Labels:                  map[string]string(reportedIbp.Labels),
			Status:                  status,
			CreatedBy:               tenant.CreatedBy,
		})
		if createErr != nil {
			return nil, fmt.Errorf("unable to create InfiniBand Partition found on Site: failed to create Partition, DB error: %w", createErr)
		}

		_, statusErr := cdbm.NewStatusDetailDAO(mibp.dbSession).Create(
			ctx,
			tx,
			cdbm.StatusDetailCreateInput{
				EntityID: created.ID.String(),
				Status:   string(status),
				Message:  &statusMessage,
			},
		)
		if statusErr != nil {
			return nil, fmt.Errorf("unable to create InfiniBand Partition found on Site: failed to create Status Detail, DB error: %w", statusErr)
		}

		return created, nil
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to recover InfiniBand Partition from Site inventory")

		return nil
	}

	return ibp
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
