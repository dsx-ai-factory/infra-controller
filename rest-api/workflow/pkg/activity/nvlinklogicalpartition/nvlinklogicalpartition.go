// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvlinklogicalpartition

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"unicode/utf8"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageNVLinkLogicalPartition is an activity wrapper for managing NVLinkLogicalPartition lifecycle that allows
// injecting DB access
type ManageNVLinkLogicalPartition struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions
// UpdateNVLinkLogicalPartitionsInDB is a Temporal activity that takes a collection of NVLinkPartition data pushed by Site Agent and updates the DB
func (mnlp ManageNVLinkLogicalPartition) UpdateNVLinkLogicalPartitionsInDB(ctx context.Context, siteID uuid.UUID, nvlinkLogicalPartitionInventory *corev1.NVLinkLogicalPartitionInventory) error {
	logger := log.With().Str("Activity", "UpdateNVLinkLogicalPartitionsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	stDAO := cdbm.NewSiteDAO(mnlp.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received NVLink Logical Partition inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	if nvlinkLogicalPartitionInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	nvlinklogicalpartitionDAO := cdbm.NewNVLinkLogicalPartitionDAO(mnlp.dbSession)

	existingNVLinkLogicalPartitions, _, err := nvlinklogicalpartitionDAO.GetAll(
		ctx,
		nil,
		cdbm.NVLinkLogicalPartitionFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get existing NVLink Logical Partitions for Site from DB")
		return err
	}

	// Construct a map of Controller NVLinkPartition ID to NVLinkPartition
	existingNVLinkLogicalPartitionIDMap := make(map[string]*cdbm.NVLinkLogicalPartition)

	for _, nvllp := range existingNVLinkLogicalPartitions {
		curNVLLP := nvllp
		existingNVLinkLogicalPartitionIDMap[nvllp.ID.String()] = &curNVLLP
	}

	reportedNVLinkLogicalPartitionIDMap := map[uuid.UUID]bool{}

	if nvlinkLogicalPartitionInventory.InventoryPage != nil {
		logger.Info().Msgf("Received NVLink Logical Partition inventory page: %d of %d, page size: %d, total count: %d",
			nvlinkLogicalPartitionInventory.InventoryPage.CurrentPage, nvlinkLogicalPartitionInventory.InventoryPage.TotalPages,
			nvlinkLogicalPartitionInventory.InventoryPage.PageSize, nvlinkLogicalPartitionInventory.InventoryPage.TotalItems)

		for _, strId := range nvlinkLogicalPartitionInventory.InventoryPage.ItemIds {
			id, err := uuid.Parse(strId)
			if err != nil {
				logger.Error().Err(err).Str("ID", strId).Msg("failed to parse NVLink Logical Partition ID from inventory page")
				continue
			}
			reportedNVLinkLogicalPartitionIDMap[id] = true
		}
	}

	statusDetailDAO := cdbm.NewStatusDetailDAO(mnlp.dbSession)
	// Iterate through NVLinkPartition Inventory and update DB
	for _, controllerNvllp := range nvlinkLogicalPartitionInventory.Partitions {
		slogger := logger.With().Str("NVLink Logical Partition ID", controllerNvllp.Id.Value).Logger()

		nvllp, ok := existingNVLinkLogicalPartitionIDMap[controllerNvllp.Id.Value]
		if !ok {
			nvllp = mnlp.createOrUpdateNVLinkLogicalPartitionFromSite(ctx, site, controllerNvllp)
			if nvllp == nil {
				continue
			}

			existingNVLinkLogicalPartitionIDMap[nvllp.ID.String()] = nvllp
			slogger.Info().Str("NVLink Logical Partition ID", nvllp.ID.String()).Msg("created or undeleted NVLink Logical Partition from Site inventory")
		}

		reportedNVLinkLogicalPartitionIDMap[nvllp.ID] = true

		var name *string
		var description *string
		var status *cdbm.NVLinkLogicalPartitionStatus
		var statusMessage *string

		// Reset missing flag if necessary
		var isMissingOnSite *bool
		if nvllp.IsMissingOnSite {
			isMissingOnSite = cutil.GetPtr(false)
		}

		if controllerNvllp.Config != nil && controllerNvllp.Config.Metadata != nil {
			siteName := controllerNvllp.Config.Metadata.Name
			if siteName != nvllp.Name {
				// Adopt the Site name only if the API would accept it. Otherwise this would undo the
				// name chosen during recovery, or fail validation and block the rest of this update.
				usable := isValidNVLinkLogicalPartitionName(siteName)
				if usable {
					taken, terr := isNVLinkLogicalPartitionNameTaken(ctx, nil, nvlinklogicalpartitionDAO, nvllp.TenantID, siteName)
					if terr != nil {
						slogger.Error().Err(terr).Msg("failed to check NVLink Logical Partition name uniqueness in DB")
						continue
					}

					usable = !taken
				}

				if usable {
					name = cutil.GetPtr(siteName)
				} else {
					slogger.Debug().Str("Site Name", siteName).Msg("keeping NVLink Logical Partition name, Site name is invalid or used by another Partition of the Tenant")
				}
			}

			if nvllp.Description == nil || (nvllp.Description != nil && controllerNvllp.Config.Metadata.Description != *nvllp.Description) {
				description = cutil.GetPtr(controllerNvllp.Config.Metadata.Description)
			}
		}

		// Update status if necessary
		if controllerNvllp.Status != nil {
			if nvllp.Status == cdbm.NVLinkLogicalPartitionStatusDeleting {
				continue
			}

			var mapped cdbm.NVLinkLogicalPartitionStatus
			mapped.FromProto(controllerNvllp.Status.State)
			if mapped != "" && mapped != nvllp.Status {
				status = &mapped
				message := mapped.Message()
				statusMessage = &message
			}
		}

		needsUpdate := name != nil || description != nil || isMissingOnSite != nil || status != nil

		// A row written since the Site collected this inventory holds changes the snapshot cannot
		// know about, including any made through the API, so writing the reported values over
		// them would lose those edits.
		if needsUpdate && site.IsTimeWithinStaleInventoryThreshold(nvllp.Updated) {
			slogger.Info().Msg("not updating NVLinkLogicalPartition yet because it changed more recently than the inventory interval")

			continue
		}

		if needsUpdate {
			_, err := nvlinklogicalpartitionDAO.Update(
				ctx,
				nil,
				cdbm.NVLinkLogicalPartitionUpdateInput{
					NVLinkLogicalPartitionID: nvllp.ID,
					Name:                     name,
					Description:              description,
					IsMissingOnSite:          isMissingOnSite,
					Status:                   status,
				},
			)

			if err != nil {
				slogger.Error().Err(err).Msg("failed to update NVLink Logical Partition data in DB")
				continue
			}

		}

		if status != nil {
			_, err = statusDetailDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: nvllp.ID.String(), Status: string(*status), Message: statusMessage})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to create status detail for NVLink Logical Partition in DB")
				continue
			}
		}

	}

	// Populate list of ibps that were not found
	nvlinklogicalpartitionsToDelete := []*cdbm.NVLinkLogicalPartition{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if util.ShouldReconcileDeletions(nvlinkLogicalPartitionInventory.GetInventoryPage()) {
		for _, nvllp := range existingNVLinkLogicalPartitionIDMap {
			found := false

			_, found = reportedNVLinkLogicalPartitionIDMap[nvllp.ID]
			if !found {
				// The NVLinkLogicalPartition was not found in the NVLinkLogicalPartition Inventory, so add it to list of NVLinkLogicalPartition to potentially delete
				nvlinklogicalpartitionsToDelete = append(nvlinklogicalpartitionsToDelete, nvllp)
			}
		}
	}

	// Loop through nvllp for deletion
	for _, nvllp := range nvlinklogicalpartitionsToDelete {
		slogger := logger.With().Str("NVLink Logical Partition ID", nvllp.ID.String()).Logger()

		if site.IsTimeWithinStaleInventoryThreshold(nvllp.Updated) {
			continue
		}

		// If the NVLinkLogicalPartition was already being deleted, we can proceed with removing it from the DB
		if nvllp.Status == cdbm.NVLinkLogicalPartitionStatusDeleting {
			err := nvlinklogicalpartitionDAO.Delete(ctx, nil, nvllp.ID)
			if err != nil {
				slogger.Error().Err(err).Msg("failed to delete NVLink Logical Partition from DB")
			}
		} else {
			// Set isMissingOnSite flag to true and update status, user can decide on deletion
			_, err := nvlinklogicalpartitionDAO.Update(
				ctx,
				nil,
				cdbm.NVLinkLogicalPartitionUpdateInput{
					NVLinkLogicalPartitionID: nvllp.ID,
					IsMissingOnSite:          cutil.GetPtr(true),
				},
			)
			if err != nil {
				slogger.Error().Err(err).Msg("failed to set missing on Site flag in DB for NVLink Logical Partition")
				continue
			}

			_, _, err = util.UpdateNVLinkLogicalPartitionStatusInDB(ctx, nil, mnlp.dbSession, nvllp.ID, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusError), cutil.GetPtr("NVLink Logical Partition is missing on Site"))
			if err != nil {
				slogger.Error().Err(err).Msg("failed to update NVLink Logical Partition status detail in DB")
			}
		}
	}

	return nil
}

// createOrUpdateNVLinkLogicalPartitionFromSite creates a REST NVLink Logical Partition
// from Site inventory, or undeletes a matching soft-deleted row. Returns nil when
// skipped or on failure.
//
//nolint:cyclop,dupl,exhaustruct,funlen,gocognit,gocyclo,maintidx,nestif,nilnil,varnamelen // Recovery is one transactional flow, matching InfiniBand Partition recovery.
func (mnlp ManageNVLinkLogicalPartition) createOrUpdateNVLinkLogicalPartitionFromSite(
	ctx context.Context,
	site *cdbm.Site,
	controllerNvllp *corev1.NVLinkLogicalPartition,
) *cdbm.NVLinkLogicalPartition {
	logger := log.With().
		Str("Activity", "UpdateNVLinkLogicalPartitionsInDB").
		Str("Site ID", site.ID.String()).
		Str("NVLink Logical Partition ID", controllerNvllp.GetId().GetValue()).
		Logger()

	controllerNvllpID, err := uuid.Parse(controllerNvllp.GetId().GetValue())
	if err != nil {
		logger.Warn().Msgf("unable to create NVLink Logical Partition found on Site: failed to parse ID, not a valid UUID %s", controllerNvllp.GetId().GetValue())

		return nil
	}

	reportedNvllp := new(cdbm.NVLinkLogicalPartition)
	reportedNvllp.FromProto(controllerNvllp)

	if reportedNvllp.Org == "" {
		logger.Warn().Msg("unable to create NVLink Logical Partition found on Site: Partition is reporting empty Tenant organization ID")

		return nil
	}

	if reportedNvllp.Name == "" {
		reportedNvllp.Name = "recovered-" + controllerNvllpID.String()[:8]
	}

	recoveredStatus := cdbm.NVLinkLogicalPartitionStatusReady
	if controllerNvllp.GetStatus() != nil {
		reportedState := controllerNvllp.GetStatus().GetState()
		if reportedState == corev1.TenantState_TERMINATING || reportedState == corev1.TenantState_TERMINATED {
			logger.Info().Msgf("skipping create or undelete of NVLink Logical Partition from Site inventory: Site reports state %s", reportedState)

			return nil
		}

		mappedStatus := cdbm.NVLinkLogicalPartitionStatus("")
		mappedStatus.FromProto(reportedState)
		if mappedStatus == "" {
			logger.Warn().Msgf("unable to create NVLink Logical Partition found on Site: unsupported state %s", reportedState)

			return nil
		}

		recoveredStatus = mappedStatus
	}

	statusMessage := "NVLink Logical Partition was found on Site"
	if recoveredStatus == cdbm.NVLinkLogicalPartitionStatusReady {
		statusMessage += ", ready for use"
	}

	nvllp, err := cdb.WithTxResult(ctx, mnlp.dbSession, func(tx *cdb.Tx) (*cdbm.NVLinkLogicalPartition, error) {
		nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(mnlp.dbSession)

		tenants, _, tenantErr := cdbm.NewTenantDAO(mnlp.dbSession).GetAll(
			ctx,
			tx,
			cdbm.TenantFilterInput{Orgs: []string{reportedNvllp.Org}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if tenantErr != nil {
			return nil, fmt.Errorf("unable to create NVLink Logical Partition found on Site: failed to retrieve Tenant by organization, DB error: %w", tenantErr)
		}

		if len(tenants) == 0 {
			logger.Warn().Msgf("unable to create NVLink Logical Partition found on Site: no Tenants were found for org: %s", reportedNvllp.Org)

			return nil, nil
		}

		tenant := &tenants[0]

		_, tenantSiteErr := cdbm.NewTenantSiteDAO(mnlp.dbSession).GetByTenantIDAndSiteID(ctx, tx, tenant.ID, site.ID, nil)
		if tenantSiteErr != nil {
			if errors.Is(tenantSiteErr, cdb.ErrDoesNotExist) {
				logger.Warn().Msgf("unable to create NVLink Logical Partition found on Site: Tenant for org %s does not have access to Site", reportedNvllp.Org)

				return nil, nil
			}

			return nil, fmt.Errorf("unable to create NVLink Logical Partition found on Site: failed to validate Tenant access to Site, DB error: %w", tenantSiteErr)
		}

		// Names are unique per Tenant across Sites, so recoveries for one Tenant must not
		// pick names concurrently even when they come from different Sites.
		lockErr := tx.TryAcquireAdvisoryLock(
			ctx,
			cdb.GetAdvisoryLockIDFromString(
				fmt.Sprintf("nvlink-logical-partition-recovery-%s", tenant.ID),
			),
			nil,
		)
		if lockErr != nil {
			return nil, fmt.Errorf("unable to create NVLink Logical Partition found on Site: failed to acquire recovery lock, DB error: %w", lockErr)
		}

		matches, _, reloadErr := nvllpDAO.GetAll(
			ctx,
			tx,
			cdbm.NVLinkLogicalPartitionFilterInput{
				NVLinkLogicalPartitionIDs: []uuid.UUID{controllerNvllpID},
				IncludeDeleted:            true,
			},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if reloadErr != nil {
			return nil, fmt.Errorf("unable to create NVLink Logical Partition found on Site: failed to retrieve Partition by ID, DB error: %w", reloadErr)
		}

		var existingNvllp *cdbm.NVLinkLogicalPartition
		if len(matches) > 0 {
			existingNvllp = &matches[0]
			if existingNvllp.SiteID != site.ID {
				logger.Warn().Msg("unable to create NVLink Logical Partition found on Site: Partition ID belongs to a different Site in REST cache")

				return nil, nil
			}

			if existingNvllp.TenantID != tenant.ID || existingNvllp.Org != reportedNvllp.Org {
				logger.Warn().Msgf("unable to create NVLink Logical Partition found on Site: tenant organization differs in REST cache and Site record %s", reportedNvllp.Org)

				return nil, nil
			}

			if existingNvllp.Deleted == nil {
				return existingNvllp, nil
			}

			if site.IsTimeWithinStaleInventoryThreshold(*existingNvllp.Deleted) {
				logger.Info().Msgf("not undeleting NVLink Logical Partition %s yet because it was deleted more recently than the inventory interval", controllerNvllpID)

				return nil, nil
			}

			if reportedNvllp.Name == existingNvllp.ID.String() {
				reportedNvllp.Name = existingNvllp.Name
			}
		}

		baseName := reportedNvllp.Name
		if !isValidNVLinkLogicalPartitionName(baseName) {
			baseName = "recovered-" + controllerNvllpID.String()[:8]
		}

		reportedNvllp.Name = baseName
		for attempt := 1; ; attempt++ {
			taken, nameErr := isNVLinkLogicalPartitionNameTaken(ctx, tx, nvllpDAO, tenant.ID, reportedNvllp.Name)
			if nameErr != nil {
				return nil, fmt.Errorf("unable to create NVLink Logical Partition found on Site: failed to retrieve Partition by name, DB error: %w", nameErr)
			}

			if !taken {
				break
			}

			suffix := "-recovered-" + controllerNvllpID.String()[:8]
			if attempt > 1 {
				suffix = fmt.Sprintf("%s-%d", suffix, attempt)
			}

			maxBaseRunes := cdbm.NVLinkLogicalPartitionNameMaxLength - len([]rune(suffix))
			nameRunes := []rune(baseName)
			if len(nameRunes) > maxBaseRunes {
				nameRunes = nameRunes[:maxBaseRunes]
			}

			reportedNvllp.Name = string(nameRunes) + suffix
		}

		if existingNvllp != nil {
			restored, clearErr := nvllpDAO.Clear(ctx, tx, cdbm.NVLinkLogicalPartitionClearInput{
				NVLinkLogicalPartitionID: existingNvllp.ID,
				Description:              reportedNvllp.Description == nil,
				Deleted:                  true,
			})
			if clearErr != nil {
				return nil, fmt.Errorf("unable to create NVLink Logical Partition found on Site: failed to clear soft-delete timestamp, DB error: %w", clearErr)
			}

			restored, updateErr := nvllpDAO.Update(ctx, tx, cdbm.NVLinkLogicalPartitionUpdateInput{
				NVLinkLogicalPartitionID: restored.ID,
				Name:                     &reportedNvllp.Name,
				Description:              reportedNvllp.Description,
				Status:                   &recoveredStatus,
				IsMissingOnSite:          cutil.GetPtr(false),
			})
			if updateErr != nil {
				return nil, fmt.Errorf("unable to create NVLink Logical Partition found on Site: failed to restore Partition data, DB error: %w", updateErr)
			}

			_, statusErr := cdbm.NewStatusDetailDAO(mnlp.dbSession).Create(
				ctx,
				tx,
				cdbm.StatusDetailCreateInput{
					EntityID: restored.ID.String(),
					Status:   string(recoveredStatus),
					Message:  &statusMessage,
				},
			)
			if statusErr != nil {
				return nil, fmt.Errorf("unable to create NVLink Logical Partition found on Site: failed to create Status Detail after undelete, DB error: %w", statusErr)
			}

			return restored, nil
		}

		created, createErr := nvllpDAO.Create(ctx, tx, cdbm.NVLinkLogicalPartitionCreateInput{
			NVLinkLogicalPartitionID: &controllerNvllpID,
			Name:                     reportedNvllp.Name,
			Description:              reportedNvllp.Description,
			TenantOrg:                reportedNvllp.Org,
			SiteID:                   site.ID,
			TenantID:                 tenant.ID,
			Status:                   recoveredStatus,
			CreatedBy:                tenant.CreatedBy,
		})
		if createErr != nil {
			return nil, fmt.Errorf("unable to create NVLink Logical Partition found on Site: failed to create Partition, DB error: %w", createErr)
		}

		_, statusErr := cdbm.NewStatusDetailDAO(mnlp.dbSession).Create(
			ctx,
			tx,
			cdbm.StatusDetailCreateInput{
				EntityID: created.ID.String(),
				Status:   string(recoveredStatus),
				Message:  &statusMessage,
			},
		)
		if statusErr != nil {
			return nil, fmt.Errorf("unable to create NVLink Logical Partition found on Site: failed to create Status Detail, DB error: %w", statusErr)
		}

		return created, nil
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to recover NVLink Logical Partition from Site inventory")

		return nil
	}

	return nvllp
}

// isValidNVLinkLogicalPartitionName reports whether name passes the length and whitespace
// rules that the NVLink Logical Partition DAO enforces.
func isValidNVLinkLogicalPartitionName(name string) bool {
	nameLen := utf8.RuneCountInString(name)

	return nameLen >= cdbm.NVLinkLogicalPartitionNameMinLength &&
		nameLen <= cdbm.NVLinkLogicalPartitionNameMaxLength &&
		strings.TrimSpace(name) == name
}

// isNVLinkLogicalPartitionNameTaken reports whether an active NVLink Logical Partition of the
// Tenant already uses name. Names are unique per Tenant across Sites, matching the API handlers.
func isNVLinkLogicalPartitionNameTaken(
	ctx context.Context,
	tx *cdb.Tx,
	nvllpDAO cdbm.NVLinkLogicalPartitionDAO,
	tenantID uuid.UUID,
	name string,
) (bool, error) {
	nvllps, _, err := nvllpDAO.GetAll(
		ctx,
		tx,
		cdbm.NVLinkLogicalPartitionFilterInput{
			Names:     []string{name},
			TenantIDs: []uuid.UUID{tenantID},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(1)},
		nil,
	)
	if err != nil {
		return false, err
	}

	return len(nvllps) > 0, nil
}

// NewManageNVLinkLogicalPartition returns a new ManageNVLinkLogicalPartition activity
func NewManageNVLinkLogicalPartition(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageNVLinkLogicalPartition {
	return ManageNVLinkLogicalPartition{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
