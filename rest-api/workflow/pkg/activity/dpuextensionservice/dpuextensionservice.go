// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dpuextensionservice

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/proto"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// DpuExtensionServiceTimeFormat is the time format used on Site for version info creation time
	DpuExtensionServiceTimeFormat = "2006-01-02 15:04:05.000000 UTC"

	dpuExtensionServiceRecoveredMessage = "DPU Extension Service was found on Site, Ready for use"
)

// ManageDpuExtensionService is an activity wrapper for managing Dpu Extension Service lifecycle that allows
// injecting DB access
type ManageDpuExtensionService struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

type dpuExtensionServiceRecoveryInput struct {
	id             uuid.UUID
	name           string
	description    *string
	serviceType    string
	dpuTarget      *string
	version        *string
	versionInfo    *cdbm.DpuExtensionServiceVersionInfo
	activeVersions []string
	tenantOrg      string
	status         string
	statusMessage  string
}

type dpuExtensionServiceRecoveryResult struct {
	dpuExtensionService *cdbm.DpuExtensionService
}

// Activity functions
// UpdateDpuExtensionServicesInDB is a Temporal activity that takes a collection of Dpu Extension Service data pushed by Site Agent and updates the DB
func (mde ManageDpuExtensionService) UpdateDpuExtensionServicesInDB(ctx context.Context, siteID uuid.UUID, inventory *corev1.DpuExtensionServiceInventory) error {
	logger := log.With().Str("Activity", "UpdateDpuExtensionServicesInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("Starting activity")

	stDAO := cdbm.NewSiteDAO(mde.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(mde.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received DPU Extension Service inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	if inventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")

		if inventory.StatusMsg != "" {
			err = fmt.Errorf("Site Agent inventory collection failure: %s", inventory.StatusMsg)
		} else {
			err = errors.New("Site Agent reported unknown inventory collection failure")
		}

		return temporal.NewNonRetryableApplicationError(err.Error(), util.ErrTypeSiteAgentInventoryCollectionFailure, err)
	}

	dpuExtensionServiceDAO := cdbm.NewDpuExtensionServiceDAO(mde.dbSession)
	existingDpuExtensionServices, _, err := dpuExtensionServiceDAO.GetAll(ctx, nil, cdbm.DpuExtensionServiceFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get DPU Extension Services for Site from DB")
		return err
	}

	// Construct a map of Controller Dpu Extension Service ID to Dpu Extension Service
	existingDpuExtensionServiceIDMap := make(map[string]*cdbm.DpuExtensionService)
	for _, dpuExtensionService := range existingDpuExtensionServices {
		curDpuExtensionService := dpuExtensionService
		existingDpuExtensionServiceIDMap[dpuExtensionService.ID.String()] = &curDpuExtensionService
	}

	reportedDpuExtensionServiceIDMap := map[uuid.UUID]bool{}
	if inventory.InventoryPage != nil {
		logger.Info().Msgf("Received DPU Extension Service inventory page: %d of %d, page size: %d, total count: %d",
			inventory.InventoryPage.CurrentPage, inventory.InventoryPage.TotalPages,
			inventory.InventoryPage.PageSize, inventory.InventoryPage.TotalItems)

		for _, strId := range inventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("Controller DPU Extension Service ID", strId).Msg("failed to parse DPU Extension Service ID from inventory item IDs")
				continue
			}
			reportedDpuExtensionServiceIDMap[id] = true
		}
	}

	// Iterate through DPU Extension Service Inventory and update DB
	for _, controllerDpuExtensionService := range inventory.DpuExtensionServices {
		slogger := logger.With().Str("Controller DPU Extension Service ID", controllerDpuExtensionService.ServiceId).Logger()

		dpuExtensionService := existingDpuExtensionServiceIDMap[controllerDpuExtensionService.ServiceId]
		if dpuExtensionService == nil {
			dpuExtensionService = mde.createOrUpdateDpuExtensionServiceFromSite(ctx, site, controllerDpuExtensionService)
			if dpuExtensionService == nil {
				continue
			}

			// Keep the in-memory map in sync so later inventory entries see this service.
			existingDpuExtensionServiceIDMap[dpuExtensionService.ID.String()] = dpuExtensionService
			slogger.Info().Str("DPU Extension Service ID", dpuExtensionService.ID.String()).Msg("created or undeleted DPU Extension Service from Site inventory")
		}

		reportedDpuExtensionServiceIDMap[dpuExtensionService.ID] = true

		var status *string
		var statusMessage *string
		var isMissingOnSite *bool

		// Update DPU Extension Service status if necessary
		if dpuExtensionService.Status == cdbm.DpuExtensionServiceStatusPending {
			// If the DPU Extension Service is in Pending status, set it to Ready
			status = cutil.GetPtr(cdbm.DpuExtensionServiceStatusReady)
			statusMessage = cutil.GetPtr("DPU Extension Service is ready for deployment")
		} else if dpuExtensionService.IsMissingOnSite && dpuExtensionService.Status == cdbm.DpuExtensionServiceStatusError {
			// If the DPU Extension Service was previously missing on Site, set it back to Ready
			status = cutil.GetPtr(cdbm.DpuExtensionServiceStatusReady)
			statusMessage = cutil.GetPtr("DPU Extension Service was re-detected on Site")
			isMissingOnSite = cutil.GetPtr(false)
		}

		var version *string
		var versionInfo *cdbm.DpuExtensionServiceVersionInfo

		if controllerDpuExtensionService.LatestVersionInfo != nil {
			latestVersion := controllerDpuExtensionService.LatestVersionInfo.Version
			data := controllerDpuExtensionService.LatestVersionInfo.Data
			hasCredentials := controllerDpuExtensionService.LatestVersionInfo.HasCredential
			controllerObservability := controllerDpuExtensionService.GetLatestVersionInfo().Observability
			var dbObservability *corev1.DpuExtensionServiceObservability
			if dpuExtensionService.VersionInfo != nil && dpuExtensionService.VersionInfo.Observability != nil {
				dbObservability = dpuExtensionService.VersionInfo.Observability.DpuExtensionServiceObservability
			}

			created, err := time.Parse(DpuExtensionServiceTimeFormat, controllerDpuExtensionService.LatestVersionInfo.Created)
			if err != nil {
				if controllerDpuExtensionService.LatestVersionInfo.Created != "" {
					slogger.Error().Err(err).Str("Created", controllerDpuExtensionService.LatestVersionInfo.Created).Msg("failed to parse timestamp for version info")
				}
				created = dpuExtensionService.Updated
			}

			if dpuExtensionService.Version == nil || *dpuExtensionService.Version != latestVersion {
				version = cutil.GetPtr(latestVersion)
			}

			controllerVersionInfo := new(cdbm.DpuExtensionServiceVersionInfo)
			controllerVersionInfo.FromProto(controllerDpuExtensionService.GetLatestVersionInfo(), created)

			if dpuExtensionService.VersionInfo == nil ||
				dpuExtensionService.VersionInfo.Version != latestVersion ||
				dpuExtensionService.VersionInfo.Data != data ||
				dpuExtensionService.VersionInfo.HasCredentials != hasCredentials ||
				dpuExtensionService.VersionInfo.Created != controllerVersionInfo.Created ||
				!proto.Equal(dbObservability, controllerObservability) {
				versionInfo = controllerVersionInfo
			}

		}

		var activeVersions []string
		if controllerDpuExtensionService.ActiveVersions != nil && !slices.Equal(dpuExtensionService.ActiveVersions, controllerDpuExtensionService.ActiveVersions) {
			activeVersions = controllerDpuExtensionService.ActiveVersions
		}

		var dpuTarget *string
		if dpuExtensionService.ServiceType == cdbm.DpuExtensionServiceServiceTypeDpfHelmChart {
			controllerDpuTarget, targetErr := cdbm.DpuExtensionServiceDpuTargetFromProto(controllerDpuExtensionService.DpuTarget)
			if targetErr != nil {
				slogger.Error().Err(targetErr).Msg("failed to map DPU Extension Service DPU target")
			} else if controllerDpuTarget != nil &&
				(dpuExtensionService.DpuTarget == nil || *dpuExtensionService.DpuTarget != *controllerDpuTarget) {
				dpuTarget = controllerDpuTarget
			}
		}

		// Core reconciles a DPF Helm chart asynchronously, so its lifecycle state owns the
		// status and supersedes the presence-based status above. Without a usable state the
		// stored status is kept rather than inferred from the service being reported.
		if dpuExtensionService.ServiceType == cdbm.DpuExtensionServiceServiceTypeDpfHelmChart {
			status = nil
			statusMessage = nil

			updatedStatus, cerr := cdbm.DpuExtensionServiceStatusFromLifecycleStatus(controllerDpuExtensionService.LifecycleStatus)
			if cerr != nil {
				slogger.Error().Err(cerr).Msg("failed to derive DPU Extension Service status from Core lifecycle status")
			} else if updatedStatus != dpuExtensionService.Status {
				status = cutil.GetPtr(updatedStatus)
				statusMessage = cutil.GetPtr(fmt.Sprintf("Core reports DPU Extension Service in %s status", updatedStatus))
			}
		}

		needsUpdate := status != nil ||
			isMissingOnSite != nil ||
			dpuTarget != nil ||
			version != nil ||
			versionInfo != nil ||
			activeVersions != nil

		// VersionInfo carries the Data, Credentials and Observability an API update sets, so a
		// row written since the Site collected this inventory holds changes the snapshot cannot
		// know about and writing the reported values over them would lose those edits.
		if needsUpdate && site.IsTimeWithinStaleInventoryThreshold(dpuExtensionService.Updated) {
			slogger.Info().Msg("not updating DpuExtensionService yet because it changed more recently than the inventory interval")

			continue
		}

		if needsUpdate {
			_, err := dpuExtensionServiceDAO.Update(ctx, nil, cdbm.DpuExtensionServiceUpdateInput{
				DpuExtensionServiceID: dpuExtensionService.ID,
				DpuTarget:             dpuTarget,
				Version:               version,
				VersionInfo:           versionInfo,
				ActiveVersions:        activeVersions,
				Status:                status,
				IsMissingOnSite:       isMissingOnSite,
			})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to update DPU Extension Service in DB")
				continue
			}
		}

		// If status was updated, then create status detail
		if status != nil {
			_, err = sdDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: dpuExtensionService.ID.String(), Status: *status, Message: statusMessage})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to create status detail for DPU Extension Service in DB")
				continue
			}
		}
	}

	// Populate list of DPU Extension Services that were not found
	dpuExtensionServicesToDelete := []*cdbm.DpuExtensionService{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if inventory.InventoryPage == nil || inventory.InventoryPage.TotalPages == 0 || (inventory.InventoryPage.CurrentPage == inventory.InventoryPage.TotalPages) {
		for _, dpuExtensionService := range existingDpuExtensionServiceIDMap {
			if !reportedDpuExtensionServiceIDMap[dpuExtensionService.ID] {
				dpuExtensionServicesToDelete = append(dpuExtensionServicesToDelete, dpuExtensionService)
			}
		}
	}

	// Loop through DPU Extension Services for deletion
	for _, dpuExtensionService := range dpuExtensionServicesToDelete {
		slogger := logger.With().Str("DPU Extension Service ID", dpuExtensionService.ID.String()).Logger()

		// Avoid these actions if the object was updated since the inventory was received
		if site.IsTimeWithinStaleInventoryThreshold(dpuExtensionService.Updated) {
			continue
		}

		// If the DPU Extension Service was already deleting, we can proceed with removing it from the DB
		if dpuExtensionService.Status == cdbm.DpuExtensionServiceStatusDeleting {
			// The DPU Extension Service was being deleted, so delete it from DB
			err := dpuExtensionServiceDAO.Delete(ctx, nil, dpuExtensionService.ID)
			if err != nil {
				slogger.Error().Err(err).Msg("failed to delete DPU Extension Service from DB")
				continue
			}
		} else if !dpuExtensionService.IsMissingOnSite {
			// Mark DPU Extension Service as missing on Site
			_, err := dpuExtensionServiceDAO.Update(ctx, nil, cdbm.DpuExtensionServiceUpdateInput{DpuExtensionServiceID: dpuExtensionService.ID, IsMissingOnSite: cutil.GetPtr(true)})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to mark DPU Extension Service as missing on Site in DB")
				continue
			}

			// Create status detail for DPU Extension Service
			_, err = sdDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: dpuExtensionService.ID.String(), Status: cdbm.DpuExtensionServiceStatusError, Message: cutil.GetPtr("DPU Extension Service is missing on Site")})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to create status detail for DPU Extension Service in DB")
				continue
			}
		}
	}

	return nil
}

// createOrUpdateDpuExtensionServiceFromSite creates a REST DPU Extension Service from Site
// inventory, or undeletes and refreshes a matching soft-deleted row. It returns nil when
// recovery is skipped or fails.
//
//nolint:funlen // Input mapping and transaction dispatch are kept together to show the recovery flow.
func (mde ManageDpuExtensionService) createOrUpdateDpuExtensionServiceFromSite(
	ctx context.Context,
	site *cdbm.Site,
	controllerDpuExtensionService *corev1.DpuExtensionService,
) *cdbm.DpuExtensionService {
	logger := log.With().
		Str("Activity", "UpdateDpuExtensionServicesInDB").
		Str("Site ID", site.ID.String()).
		Str("DPU Extension Service ID", controllerDpuExtensionService.GetServiceId()).
		Logger()

	recoveryInput, valid := dpuExtensionServiceRecoveryInputFromSite(controllerDpuExtensionService)
	if !valid {
		return nil
	}

	result, err := cdb.WithTxResult(ctx, mde.dbSession, func(transaction *cdb.Tx) (dpuExtensionServiceRecoveryResult, error) {
		dpuExtensionServiceDAO := cdbm.NewDpuExtensionServiceDAO(mde.dbSession)

		tenant, eligible, tenantErr := mde.resolveDpuExtensionServiceRecoveryTenant(
			ctx,
			transaction,
			site,
			recoveryInput.tenantOrg,
		)
		if tenantErr != nil {
			return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, tenantErr
		}

		if !eligible {
			return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, nil
		}

		// Serialize recovery names per Tenant so concurrent inventory pages cannot choose
		// the same fallback name.
		lockErr := transaction.TryAcquireAdvisoryLock(
			ctx,
			cdb.GetAdvisoryLockIDFromString("dpu-extension-service-recovery-"+tenant.ID.String()),
			nil,
		)
		if lockErr != nil {
			return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to acquire Tenant recovery lock, DB error: %w", lockErr)
		}

		matches, _, reloadErr := dpuExtensionServiceDAO.GetAll(
			ctx,
			transaction,
			cdbm.DpuExtensionServiceFilterInput{
				DpuExtensionServiceIDs: []uuid.UUID{recoveryInput.id},
				IncludeDeleted:         true,
			},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if reloadErr != nil {
			return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to retrieve service by ID, DB error: %w", reloadErr)
		}

		if len(matches) > 0 {
			return mde.restoreDpuExtensionServiceFromSite(
				ctx,
				transaction,
				dpuExtensionServiceDAO,
				site,
				tenant,
				&matches[0],
				recoveryInput,
			)
		}

		return mde.createDpuExtensionServiceFromSite(
			ctx,
			transaction,
			dpuExtensionServiceDAO,
			site,
			tenant,
			recoveryInput,
		)
	})
	if err != nil {
		logger.Warn().Err(err).Msg("failed to create or undelete DPU Extension Service from Site inventory")

		return nil
	}

	return result.dpuExtensionService
}

func dpuExtensionServiceRecoveryInputFromSite(
	controllerDpuExtensionService *corev1.DpuExtensionService,
) (dpuExtensionServiceRecoveryInput, bool) {
	logger := log.With().
		Str("Activity", "UpdateDpuExtensionServicesInDB").
		Str("DPU Extension Service ID", controllerDpuExtensionService.GetServiceId()).
		Logger()

	dpuExtensionServiceID, err := uuid.Parse(controllerDpuExtensionService.GetServiceId())
	if err != nil {
		logger.Warn().Msgf("unable to create DPU Extension Service found on Site: failed to parse ID, not a valid UUID %s", controllerDpuExtensionService.GetServiceId())

		return dpuExtensionServiceRecoveryInput{}, false
	}

	org := controllerDpuExtensionService.GetTenantOrganizationId()
	if org == "" {
		logger.Warn().Msg("unable to create DPU Extension Service found on Site: service is reporting empty Tenant organization ID")

		return dpuExtensionServiceRecoveryInput{}, false
	}

	serviceType := cdbm.DpuExtensionServiceServiceTypeKubernetesPod
	status := cdbm.DpuExtensionServiceStatusReady
	statusMessage := dpuExtensionServiceRecoveredMessage
	var dpuTarget *string

	switch controllerDpuExtensionService.GetServiceType() {
	case corev1.DpuExtensionServiceType_KUBERNETES_POD:
	case corev1.DpuExtensionServiceType_DPF_HELM_CHART:
		serviceType = cdbm.DpuExtensionServiceServiceTypeDpfHelmChart

		var targetErr error
		dpuTarget, targetErr = cdbm.DpuExtensionServiceDpuTargetFromProto(controllerDpuExtensionService.DpuTarget)
		if targetErr != nil || dpuTarget == nil {
			logger.Warn().Err(targetErr).Msg("unable to create DPU Extension Service found on Site: DPF Helm chart is missing a valid DPU target")

			return dpuExtensionServiceRecoveryInput{}, false
		}

		status = cdbm.DpuExtensionServiceStatusPending
		statusMessage = "DPU Extension Service was found on Site, pending DPF reconciliation"
		lifecycleStatus, lifecycleErr := cdbm.DpuExtensionServiceStatusFromLifecycleStatus(controllerDpuExtensionService.LifecycleStatus)
		if lifecycleErr == nil {
			status = lifecycleStatus
			statusMessage = fmt.Sprintf("Core reports DPU Extension Service in %s status", lifecycleStatus)
		}
	default:
		logger.Warn().Msgf("unable to create DPU Extension Service found on Site: unsupported service type %s", controllerDpuExtensionService.GetServiceType())

		return dpuExtensionServiceRecoveryInput{}, false
	}

	name := controllerDpuExtensionService.GetServiceName()
	if name == "" {
		name = "recovered-" + dpuExtensionServiceID.String()[:8]
	}

	var description *string
	if controllerDpuExtensionService.GetDescription() != "" {
		description = cutil.GetPtr(controllerDpuExtensionService.GetDescription())
	}

	var (
		version     *string
		versionInfo *cdbm.DpuExtensionServiceVersionInfo
	)
	if latestVersionInfo := controllerDpuExtensionService.GetLatestVersionInfo(); latestVersionInfo != nil {
		version = cutil.GetPtr(latestVersionInfo.GetVersion())
		versionInfo = new(cdbm.DpuExtensionServiceVersionInfo)
		versionInfo.FromProto(latestVersionInfo, time.Now().UTC())
	}

	return dpuExtensionServiceRecoveryInput{
		id:             dpuExtensionServiceID,
		name:           name,
		description:    description,
		serviceType:    serviceType,
		dpuTarget:      dpuTarget,
		version:        version,
		versionInfo:    versionInfo,
		activeVersions: append([]string{}, controllerDpuExtensionService.GetActiveVersions()...),
		tenantOrg:      org,
		status:         status,
		statusMessage:  statusMessage,
	}, true
}

func (mde ManageDpuExtensionService) resolveDpuExtensionServiceRecoveryTenant(
	ctx context.Context,
	transaction *cdb.Tx,
	site *cdbm.Site,
	org string,
) (*cdbm.Tenant, bool, error) {
	logger := log.With().
		Str("Activity", "UpdateDpuExtensionServicesInDB").
		Str("Site ID", site.ID.String()).
		Logger()

	tenants, _, err := cdbm.NewTenantDAO(mde.dbSession).GetAll(
		ctx,
		transaction,
		cdbm.TenantFilterInput{Orgs: []string{org}},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		return nil, false, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to retrieve Tenant by organization, DB error: %w", err)
	}

	if len(tenants) == 0 {
		logger.Warn().Msgf("unable to create DPU Extension Service found on Site: no Tenants were found for org: %s", org)

		return nil, false, nil
	}

	if len(tenants) > 1 {
		logger.Warn().Msgf("unable to create DPU Extension Service found on Site: multiple Tenants were found for org: %s", org)

		return nil, false, nil
	}

	tenant := &tenants[0]

	tenantSites, _, err := cdbm.NewTenantSiteDAO(mde.dbSession).GetAll(
		ctx,
		transaction,
		cdbm.TenantSiteFilterInput{
			TenantIDs: []uuid.UUID{tenant.ID},
			SiteIDs:   []uuid.UUID{site.ID},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(1)},
		nil,
	)
	if err != nil {
		return nil, false, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to validate Tenant access to Site, DB error: %w", err)
	}

	if len(tenantSites) == 0 {
		logger.Warn().Msgf("unable to create DPU Extension Service found on Site: Tenant for org %s does not have access to Site", org)

		return nil, false, nil
	}

	return tenant, true, nil
}

//nolint:funlen // Validation, undelete, status update, and status history must stay in one transaction.
func (mde ManageDpuExtensionService) restoreDpuExtensionServiceFromSite(
	ctx context.Context,
	transaction *cdb.Tx,
	dpuExtensionServiceDAO cdbm.DpuExtensionServiceDAO,
	site *cdbm.Site,
	tenant *cdbm.Tenant,
	existingDpuExtensionService *cdbm.DpuExtensionService,
	recoveryInput dpuExtensionServiceRecoveryInput,
) (dpuExtensionServiceRecoveryResult, error) {
	logger := log.With().
		Str("Activity", "UpdateDpuExtensionServicesInDB").
		Str("Site ID", site.ID.String()).
		Str("DPU Extension Service ID", recoveryInput.id.String()).
		Logger()

	if existingDpuExtensionService.SiteID != site.ID {
		logger.Warn().Msg("unable to create DPU Extension Service found on Site: service ID belongs to a different Site in REST cache")

		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, nil
	}

	if existingDpuExtensionService.TenantID != tenant.ID {
		logger.Warn().Msgf("unable to create DPU Extension Service found on Site: tenant organization differs in REST cache and Site record %s", recoveryInput.tenantOrg)

		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, nil
	}

	if existingDpuExtensionService.ServiceType != recoveryInput.serviceType {
		logger.Warn().Msg("unable to create DPU Extension Service found on Site: service type differs in REST cache and Site record")

		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, nil
	}
	if existingDpuExtensionService.DpuTarget != nil &&
		!util.PtrsEqual(existingDpuExtensionService.DpuTarget, recoveryInput.dpuTarget) {
		logger.Warn().Msg("unable to create DPU Extension Service found on Site: DPU target differs in REST cache and Site record")

		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, nil
	}

	if existingDpuExtensionService.Deleted == nil {
		return dpuExtensionServiceRecoveryResult{dpuExtensionService: existingDpuExtensionService}, nil
	}

	// A delete newer than the inventory staleness threshold can postdate this snapshot.
	// A later inventory restores the service if Site still reports it.
	if site.IsTimeWithinStaleInventoryThreshold(*existingDpuExtensionService.Deleted) {
		logger.Info().Msgf("not undeleting DPU Extension Service %s yet because it was deleted more recently than the inventory interval", recoveryInput.id)

		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, nil
	}

	recoveredName, err := availableDpuExtensionServiceName(
		ctx,
		transaction,
		dpuExtensionServiceDAO,
		tenant.ID,
		recoveryInput.id,
		recoveryInput.name,
	)
	if err != nil {
		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to select recovery name, DB error: %w", err)
	}

	restored, err := dpuExtensionServiceDAO.Clear(
		ctx,
		transaction,
		cdbm.DpuExtensionServiceClearInput{
			DpuExtensionServiceID: existingDpuExtensionService.ID,
			Deleted:               true,
		},
	)
	if err != nil {
		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to clear soft-delete timestamp, DB error: %w", err)
	}

	restored, err = dpuExtensionServiceDAO.Update(
		ctx,
		transaction,
		cdbm.DpuExtensionServiceUpdateInput{
			DpuExtensionServiceID: restored.ID,
			Name:                  &recoveredName,
			DpuTarget:             recoveryInput.dpuTarget,
			Version:               recoveryInput.version,
			VersionInfo:           recoveryInput.versionInfo,
			ActiveVersions:        recoveryInput.activeVersions,
			Status:                &recoveryInput.status,
			IsMissingOnSite:       cutil.GetPtr(false),
		},
	)
	if err != nil {
		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to restore service status, DB error: %w", err)
	}

	_, err = cdbm.NewStatusDetailDAO(mde.dbSession).Create(
		ctx,
		transaction,
		cdbm.StatusDetailCreateInput{
			EntityID: restored.ID.String(),
			Status:   recoveryInput.status,
			Message:  cutil.GetPtr(recoveryInput.statusMessage),
		},
	)
	if err != nil {
		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to create Status Detail after undelete, DB error: %w", err)
	}

	return dpuExtensionServiceRecoveryResult{dpuExtensionService: restored}, nil
}

func (mde ManageDpuExtensionService) createDpuExtensionServiceFromSite(
	ctx context.Context,
	transaction *cdb.Tx,
	dpuExtensionServiceDAO cdbm.DpuExtensionServiceDAO,
	site *cdbm.Site,
	tenant *cdbm.Tenant,
	recoveryInput dpuExtensionServiceRecoveryInput,
) (dpuExtensionServiceRecoveryResult, error) {
	recoveredName, err := availableDpuExtensionServiceName(
		ctx,
		transaction,
		dpuExtensionServiceDAO,
		tenant.ID,
		recoveryInput.id,
		recoveryInput.name,
	)
	if err != nil {
		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to select recovery name, DB error: %w", err)
	}

	created, err := dpuExtensionServiceDAO.Create(
		ctx,
		transaction,
		cdbm.DpuExtensionServiceCreateInput{
			DpuExtensionServiceID: &recoveryInput.id,
			Name:                  recoveredName,
			Description:           recoveryInput.description,
			ServiceType:           recoveryInput.serviceType,
			DpuTarget:             recoveryInput.dpuTarget,
			SiteID:                site.ID,
			TenantID:              tenant.ID,
			Version:               recoveryInput.version,
			VersionInfo:           recoveryInput.versionInfo,
			ActiveVersions:        recoveryInput.activeVersions,
			Status:                recoveryInput.status,
			CreatedBy:             tenant.CreatedBy,
		},
	)
	if err != nil {
		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to create service, DB error: %w", err)
	}

	_, err = cdbm.NewStatusDetailDAO(mde.dbSession).Create(
		ctx,
		transaction,
		cdbm.StatusDetailCreateInput{
			EntityID: created.ID.String(),
			Status:   recoveryInput.status,
			Message:  cutil.GetPtr(recoveryInput.statusMessage),
		},
	)
	if err != nil {
		return dpuExtensionServiceRecoveryResult{dpuExtensionService: nil}, fmt.Errorf("unable to create DPU Extension Service found on Site: failed to create Status Detail, DB error: %w", err)
	}

	return dpuExtensionServiceRecoveryResult{dpuExtensionService: created}, nil
}

// availableDpuExtensionServiceName returns an API-valid, tenant-unique name for a service
// recovered from Site inventory. The caller must hold the Tenant recovery advisory lock.
func availableDpuExtensionServiceName(
	ctx context.Context,
	transaction *cdb.Tx,
	dpuExtensionServiceDAO cdbm.DpuExtensionServiceDAO,
	tenantID uuid.UUID,
	dpuExtensionServiceID uuid.UUID,
	reportedName string,
) (string, error) {
	const maxNameLength = 256

	baseName := reportedName
	baseNameRunes := []rune(baseName)

	if len(baseNameRunes) < 2 ||
		len(baseNameRunes) > maxNameLength ||
		strings.TrimSpace(baseName) != baseName {
		baseName = "recovered-" + dpuExtensionServiceID.String()[:8]
	}

	candidate := baseName
	for attempt := 1; ; attempt++ {
		nameConflicts, _, err := dpuExtensionServiceDAO.GetAll(
			ctx,
			transaction,
			cdbm.DpuExtensionServiceFilterInput{
				Names:     []string{candidate},
				TenantIDs: []uuid.UUID{tenantID},
			},
			cdbp.PageInput{Limit: cutil.GetPtr(1)},
			nil,
		)
		if err != nil {
			return "", fmt.Errorf("failed to query DPU Extension Service name conflicts: %w", err)
		}

		if len(nameConflicts) == 0 {
			return candidate, nil
		}

		suffix := "-recovered-" + dpuExtensionServiceID.String()[:8]
		if attempt > 1 {
			suffix = fmt.Sprintf("%s-%d", suffix, attempt)
		}

		maxBaseRunes := maxNameLength - len([]rune(suffix))
		runes := []rune(baseName)

		if len(runes) > maxBaseRunes {
			runes = runes[:maxBaseRunes]
		}

		candidate = string(runes) + suffix
	}
}

// NewManageDpuExtensionService returns a new ManageDpuExtensionService activity
func NewManageDpuExtensionService(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageDpuExtensionService {
	return ManageDpuExtensionService{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
