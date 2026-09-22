// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpc

import (
	"context"
	"errors"
	"fmt"
	"reflect"
	"slices"
	"time"

	"github.com/google/uuid"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/client"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ManageVpc is an activity wrapper for managing VPC lifecycle that allows
// injecting DB access
type ManageVpc struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
	tc             client.Client
}

// Activity functions

// UpdateVpcsInDB is a Temporal activity that takes a collection of VPC data pushed by Site Agent and updates the DB
func (mv ManageVpc) UpdateVpcsInDB(ctx context.Context, siteID uuid.UUID, vpcInventory *corev1.VPCInventory) ([]cwm.InventoryObjectLifecycleEvent, error) {
	logger := log.With().Str("Activity", "UpdateVpcsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	// Initialize metrics tracking variables
	vpcLifecycleEvents := []cwm.InventoryObjectLifecycleEvent{}

	stDAO := cdbm.NewSiteDAO(mv.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received VPC inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return nil, err
	}

	// Get temporal client for specified Site
	tc, err := mv.siteClientPool.GetClientByID(siteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return nil, err
	}

	if vpcInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil, errors.New(vpcInventory.StatusMsg)
	}

	vpcDAO := cdbm.NewVpcDAO(mv.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(mv.dbSession)

	existingVpcs, _, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{
		SiteIDs: []uuid.UUID{site.ID},
	}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get VPCs for Site from DB")
		return nil, err
	}

	existingVpcIDMap := make(map[string]*cdbm.Vpc)
	existingVpcCtrlIDMap := make(map[string]*cdbm.Vpc)

	for i := range existingVpcs {
		vpc := &existingVpcs[i]
		existingVpcIDMap[vpc.ID.String()] = vpc
		if vpc.ControllerVpcID != nil {
			existingVpcCtrlIDMap[vpc.ControllerVpcID.String()] = vpc
		}
	}

	reportedVpcIDMap := map[uuid.UUID]bool{}

	if vpcInventory.InventoryPage != nil {
		logger.Info().Msgf("Received VPC inventory page: %d of %d, page size: %d, total count: %d",
			vpcInventory.InventoryPage.CurrentPage, vpcInventory.InventoryPage.TotalPages,
			vpcInventory.InventoryPage.PageSize, vpcInventory.InventoryPage.TotalItems)

		for _, strId := range vpcInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strId).Msg("failed to parse VPC ID from inventory page")
				continue
			}
			reportedVpcIDMap[id] = true
		}
	}

	// Prepare a map of ID -> propagation status
	// so we can quickly attach it to the object
	// when need to perform the update query.
	vpcPropagationStatus := map[string]*cdbm.NetworkSecurityGroupPropagationDetails{}
	for _, propStatus := range vpcInventory.NetworkSecurityGroupPropagations {
		vpcPropagationStatus[propStatus.Id] = &cdbm.NetworkSecurityGroupPropagationDetails{NetworkSecurityGroupPropagationObjectStatus: propStatus}
		logger.Debug().Str("Controller VPC ID", propStatus.Id).Msg("propagation details cached for VPC")
	}

	// Iterate through VPC Inventory and update DB
	for _, controllerVpc := range vpcInventory.Vpcs {
		if controllerVpc == nil || controllerVpc.GetId().GetValue() == "" {
			logger.Error().Msg("received VPC inventory entry with missing controller ID, skipping")
			continue
		}
		controllerVpcIDStr := controllerVpc.GetId().GetValue()
		slogger := logger.With().Str("VPC Controller ID", controllerVpcIDStr).Logger()

		sitePropagationStatus := vpcPropagationStatus[controllerVpcIDStr]
		slogger.Debug().Msgf("cached propagation status for VPC %+v", sitePropagationStatus)

		vpc := existingVpcCtrlIDMap[controllerVpcIDStr]
		if vpc == nil {
			vpc = existingVpcIDMap[controllerVpcIDStr]
		}

		// A VPC this run creates or undeletes carries the write time of that write, which the
		// staleness gate below would read as a concurrent edit and defer to.
		createdOrRestoredFromSite := false

		// No active REST row for this inventory VPC: create one or undelete a soft-deleted match,
		// then fall through so the main inventory loop applies Site-reported field updates.
		if vpc == nil {
			vpc = mv.createOrUpdateVpcFromSite(ctx, site, controllerVpc, sitePropagationStatus)
			if vpc == nil {
				continue
			}
			createdOrRestoredFromSite = true

			// Keep in-memory maps in sync so later inventory entries and missing-on-Site detection see this VPC.
			existingVpcIDMap[vpc.ID.String()] = vpc
			if vpc.ControllerVpcID != nil {
				existingVpcCtrlIDMap[vpc.ControllerVpcID.String()] = vpc
			}
			slogger.Info().Str("VPC ID", vpc.ID.String()).Msg("created or undeleted VPC from Site inventory")
		}

		reportedVpcIDMap[vpc.ID] = true

		// Reset missing flag if necessary
		var isMissingOnSite *bool
		if vpc.IsMissingOnSite {
			isMissingOnSite = cwutil.GetPtr(false)
		}

		// Populate controller VPC ID if necessary
		var controllerVpcID *uuid.UUID
		if vpc.ControllerVpcID == nil {
			ctrlID, serr := uuid.Parse(controllerVpc.Id.Value)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to parse VPC Controller ID, not a valid UUID")
				continue
			}
			controllerVpcID = &ctrlID
		}

		reportedVpc := &cdbm.Vpc{}
		reportedVpc.FromProto(controllerVpc)

		var slaacEnabled *bool
		reportedConfig := controllerVpc.GetConfig()
		if reportedConfig != nil && reportedConfig.SlaacEnabled != nil {
			reportedSlaacEnabled := reportedConfig.GetSlaacEnabled()
			if vpc.SlaacEnabled != reportedSlaacEnabled {
				slaacEnabled = cwutil.GetPtr(reportedSlaacEnabled)
			}
		}

		// Initialized Network virtualization type
		var networkVirtualizationType *string
		// If the VPC in the DB has Network Virtualization Type, but Site reported different one then update it
		reportedVirtType := reportedVpc.NetworkVirtualizationType
		if reportedVirtType != nil && !util.PtrsEqual(vpc.NetworkVirtualizationType, reportedVirtType) {
			networkVirtualizationType = reportedVirtType
		}

		controllerActiveVni := reportedVpc.ActiveVni
		reportedRoutingProfile := reportedVpc.RoutingProfile
		reportedRoutingProfileOverrides := reportedVpc.RoutingProfileOverrides
		reportedEffectiveRoutingProfile := reportedVpc.EffectiveRoutingProfile
		reportedPowerResourceGroup := reportedVpc.PowerResourceGroup
		reportedNSGID := reportedVpc.NetworkSecurityGroupID

		needsUpdate := isMissingOnSite != nil ||
			controllerVpcID != nil ||
			slaacEnabled != nil ||
			networkVirtualizationType != nil ||
			!util.PtrsEqual(vpc.RoutingProfile, reportedRoutingProfile) ||
			!reflect.DeepEqual(vpc.RoutingProfileOverrides, reportedRoutingProfileOverrides) ||
			!reflect.DeepEqual(vpc.EffectiveRoutingProfile, reportedEffectiveRoutingProfile) ||
			!util.PtrsEqual(vpc.PowerResourceGroup, reportedPowerResourceGroup) ||
			!util.PtrsEqual(vpc.NetworkSecurityGroupID, reportedNSGID) ||
			!vpc.NetworkSecurityGroupPropagationDetails.Equal(sitePropagationStatus) ||
			// Changing VNI isn't allowed after creation, and it should never go back to nil - that would be a bug.
			// We should assume status _could start_ as null and then update to the active VPC VNI.
			// Status should never go back to nil - that would be a bug.
			(controllerActiveVni != nil && !util.PtrsEqual(vpc.ActiveVni, controllerActiveVni))

		// A row written since the Site collected this inventory holds changes the snapshot cannot
		// know about, including any made through the API, so the clears and the write below would
		// lose those edits. The Site-owned fields they carry are reported again next run.
		if needsUpdate && !createdOrRestoredFromSite && site.IsTimeWithinStaleInventoryThreshold(vpc.Updated) {
			slogger.Info().Msg("not updating VPC yet because it changed more recently than the inventory interval")

			continue
		}

		if needsUpdate {
			if vpc.PowerResourceGroup != nil && reportedPowerResourceGroup == nil {
				vpc, err = vpcDAO.Clear(ctx, nil, cdbm.VpcClearInput{
					VpcID:              vpc.ID,
					PowerResourceGroup: true,
				})
				if err != nil {
					slogger.Error().Err(err).Msg("failed to clear PowerResourceGroup for VPC in DB")
					continue
				}
			}

			// A nil Update field is ignored, so explicitly clear a stale NSG association.
			if vpc.NetworkSecurityGroupID != nil && reportedNSGID == nil {
				vpc, err = vpcDAO.Clear(ctx, nil, cdbm.VpcClearInput{
					VpcID:                  vpc.ID,
					NetworkSecurityGroupID: true,
				})
				if err != nil {
					slogger.Error().Err(err).Msg("failed to clear NetworkSecurityGroupID for VPC in DB")
					continue
				}
			}

			// If the VPC in the DB has propagation details but the site reported no propagation details
			// then we should clear it in the DB.  Passing along the nil to the Update call would
			// just ignore the field.
			if vpc.NetworkSecurityGroupPropagationDetails != nil && sitePropagationStatus == nil {
				vpc, err = vpcDAO.Clear(ctx, nil, cdbm.VpcClearInput{
					VpcID:                                  vpc.ID,
					NetworkSecurityGroupPropagationDetails: true,
				})
				if err != nil {
					slogger.Error().Err(err).Msg("failed to clear NetworkSecurityGroupPropagationDetails for VPC in DB")
					continue
				}
			}

			// Controller omission clears cached routing-profile state. Update treats nil
			// as leave-unchanged, so explicitly clear each stale field first.
			if vpc.RoutingProfile != nil && reportedRoutingProfile == nil {
				vpc, err = vpcDAO.Clear(ctx, nil, cdbm.VpcClearInput{
					VpcID:          vpc.ID,
					RoutingProfile: true,
				})
				if err != nil {
					slogger.Error().Err(err).Msg("failed to clear RoutingProfile for VPC in DB")
					continue
				}
			}

			if vpc.RoutingProfileOverrides != nil && reportedRoutingProfileOverrides == nil {
				vpc, err = vpcDAO.Clear(ctx, nil, cdbm.VpcClearInput{
					VpcID:                   vpc.ID,
					RoutingProfileOverrides: true,
				})
				if err != nil {
					slogger.Error().Err(err).Msg("failed to clear RoutingProfileOverrides for VPC in DB")
					continue
				}
			}

			if vpc.EffectiveRoutingProfile != nil && reportedEffectiveRoutingProfile == nil {
				vpc, err = vpcDAO.Clear(ctx, nil, cdbm.VpcClearInput{
					VpcID:                   vpc.ID,
					EffectiveRoutingProfile: true,
				})
				if err != nil {
					slogger.Error().Err(err).Msg("failed to clear EffectiveRoutingProfile for VPC in DB")
					continue
				}
			}

			// Persist remaining non-nil controller-reported differences after explicit clears.
			_, serr := vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{
				VpcID:                                  vpc.ID,
				NetworkSecurityGroupID:                 reportedNSGID,
				NetworkSecurityGroupPropagationDetails: sitePropagationStatus,
				NetworkVirtualizationType:              networkVirtualizationType,
				SlaacEnabled:                           slaacEnabled,
				RoutingProfile:                         reportedRoutingProfile,
				RoutingProfileOverrides:                reportedRoutingProfileOverrides,
				EffectiveRoutingProfile:                reportedEffectiveRoutingProfile,
				PowerResourceGroup:                     reportedPowerResourceGroup,
				ControllerVpcID:                        controllerVpcID,
				IsMissingOnSite:                        isMissingOnSite,
				ActiveVni:                              controllerActiveVni,
			})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update missing on Site flag/controller VPC ID in DB")
				continue
			}
		}

		// If VPC is not in Deleting state, then update status to Ready
		if vpc.Status != cdbm.VpcStatusDeleting && vpc.Status != cdbm.VpcStatusReady {
			err = mv.updateVpcStatusInDB(ctx, nil, vpc.ID, cwutil.GetPtr(cdbm.VpcStatusReady), cwutil.GetPtr("VPC is ready for use"))
			if err != nil {
				slogger.Error().Err(err).Msg("failed to update VPC status detail in DB")
			}
		}

		// Verify if VPC's metadata update required, if yes trigger `UpdateVPC` workflow
		if controllerVpc.Metadata != nil {
			triggerVpcMetadataUpdate := false

			if vpc.Name != controllerVpc.Metadata.Name {
				triggerVpcMetadataUpdate = true
			}

			if vpc.Description != nil && *vpc.Description != controllerVpc.Metadata.Description {
				triggerVpcMetadataUpdate = true
			}

			if controllerVpc.Metadata.Labels != nil && vpc.Labels != nil {
				if len(vpc.Labels) != len(controllerVpc.Metadata.Labels) {
					triggerVpcMetadataUpdate = true
				} else {
					// Verify if each label matches with Vpc in cloud
					for _, label := range controllerVpc.Metadata.Labels {
						if label != nil {
							// case1: Key not found
							_, ok := vpc.Labels[label.Key]
							if !ok {
								triggerVpcMetadataUpdate = true
								break
							}

							// case2: Value isn't matching
							if label.Value != nil {
								if vpc.Labels[label.Key] != *label.Value {
									triggerVpcMetadataUpdate = true
									break
								}
							}
						}
					}
				}
			}

			// Trigger update Vpc metadata workflow
			if triggerVpcMetadataUpdate {
				_ = mv.UpdateVpcMetadata(ctx, siteID, tc, vpc.ID, controllerVpc)
			}
		}
	}

	// Populate list of VPCs that were not found
	vpcsToDelete := []*cdbm.Vpc{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if vpcInventory.InventoryPage == nil || vpcInventory.InventoryPage.TotalPages == 0 || (vpcInventory.InventoryPage.CurrentPage == vpcInventory.InventoryPage.TotalPages) {
		for _, vpc := range existingVpcIDMap {
			found := false

			_, found = reportedVpcIDMap[vpc.ID]
			if !found && vpc.ControllerVpcID != nil {
				// Additional check if controller VPC ID != VPC ID
				_, found = reportedVpcIDMap[*vpc.ControllerVpcID]
			}

			if !found {
				// The VPC was not found in the VPC Inventory, so add it to list of VPCs to potentially terminate
				vpcsToDelete = append(vpcsToDelete, vpc)
			}
		}
	}

	// Loop through VPCs for deletion
	for _, vpc := range vpcsToDelete {
		slogger := logger.With().Str("VPC ID", vpc.ID.String()).Logger()

		// If the VPC was already being deleted, we can proceed with removing it from the DB
		if vpc.Status == cdbm.VpcStatusDeleting {
			serr := vpcDAO.DeleteByID(ctx, nil, vpc.ID)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to delete VPC from DB")
			} else {
				// Add VPC ID to deletedVpcIDs list
				vpcLifecycleEvents = append(vpcLifecycleEvents, cwm.InventoryObjectLifecycleEvent{
					ObjectID: vpc.ID,
					Deleted:  cwutil.GetPtr(time.Now()),
				})
			}
		} else if vpc.ControllerVpcID != nil {
			// Was this created within inventory receipt interval? If so, we may be processing an older inventory
			if site.IsTimeWithinStaleInventoryThreshold(vpc.Created) {
				continue
			}

			status := cdbm.VpcStatusError
			statusMessage := "VPC is missing on Site"

			// Leave orderBy as nil as the result is sorted by created timestamp by default
			if status == vpc.Status {
				latestsd, _, serr := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{vpc.ID.String()}}, cdbp.PageInput{Limit: cwutil.GetPtr(1)})
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to retrieve latest Status Detail for VPC")
					continue
				}

				if len(latestsd) > 0 && latestsd[0].Message != nil && *latestsd[0].Message == statusMessage {
					continue
				}
			}

			// Set isMissingOnSite flag to true and update status, user can decide on deletion
			_, serr := vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{VpcID: vpc.ID, IsMissingOnSite: cwutil.GetPtr(true)})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to set missing on Site flag in DB")
				continue
			}

			serr = mv.updateVpcStatusInDB(ctx, nil, vpc.ID, &status, &statusMessage)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update status and/or create Status Detail in DB")
			}
		}
	}

	return vpcLifecycleEvents, nil
}

// createOrUpdateVpcFromSite creates a REST VPC from Site inventory, or undeletes
// a matching soft-deleted row. Field refresh after undelete is left to UpdateVpcsInDB.
// Returns nil when skipped or on failure.
func (mv ManageVpc) createOrUpdateVpcFromSite(
	ctx context.Context,
	site *cdbm.Site,
	controllerVpc *corev1.Vpc,
	propagationDetails *cdbm.NetworkSecurityGroupPropagationDetails,
) *cdbm.Vpc {
	logger := log.With().
		Str("Activity", "UpdateVpcsInDB").
		Str("Site ID", site.ID.String()).
		Str("VPC Controller ID", controllerVpc.GetId().GetValue()).
		Logger()

	vpcID, err := uuid.Parse(controllerVpc.GetId().GetValue())
	if err != nil {
		logger.Warn().Msg(fmt.Sprintf("unable to create VPC found on Site: failed to parse VPC Controller ID, not a valid UUID %s", controllerVpc.GetId().GetValue()))
		return nil
	}

	reportedVpc := new(cdbm.Vpc)
	reportedVpc.FromProto(controllerVpc)
	if reportedVpc.Name == "" {
		reportedVpc.Name = fmt.Sprintf("recovered-%s", vpcID.String()[:8])
	}
	if reportedVpc.Org == "" {
		logger.Warn().Msg("unable to create VPC found on Site: VPC on Site is reporting empty Tenant organization ID")
		return nil
	}

	// Create/undelete under one transaction so concurrent inventory pages cannot insert duplicates.
	vpc, err := cdb.WithTxResult(ctx, mv.dbSession, func(tx *cdb.Tx) (*cdbm.Vpc, error) {
		vpcDAO := cdbm.NewVpcDAO(mv.dbSession)

		// ID and ControllerVpcID are aligned, so primary-key lookup is sufficient.
		matches, _, reloadErr := vpcDAO.GetAll(ctx, tx, cdbm.VpcFilterInput{
			VpcIDs: []uuid.UUID{vpcID}, SiteIDs: []uuid.UUID{site.ID}, IncludeDeleted: true,
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if reloadErr != nil {
			return nil, fmt.Errorf("unable to create VPC found on Site: failed to retrieve VPC by controller ID, DB error: %w", reloadErr)
		}

		if len(matches) > 0 {
			existingVpc := &matches[0]
			if existingVpc.Deleted == nil {
				return existingVpc, nil
			}
			if existingVpc.Org != reportedVpc.Org {
				logger.Warn().Msg(fmt.Sprintf("unable to create VPC found on Site: tenant organization differs in REST cache and Site record %s", reportedVpc.Org))
				return nil, nil
			}
			// Deleted records when the delete happened, so a delete newer than the interval can
			// postdate this inventory. Undeleting then would revive a VPC the snapshot never saw
			// removed. A later inventory undeletes it if the Site still reports it.
			if site.IsTimeWithinStaleInventoryThreshold(*existingVpc.Deleted) {
				logger.Info().Msgf("not undeleting VPC %s yet because it was deleted more recently than the inventory interval", vpcID)
				return nil, nil
			}

			// Undelete only; UpdateVpcsInDB applies Site-reported field updates.
			restored, clearErr := vpcDAO.Clear(ctx, tx, cdbm.VpcClearInput{VpcID: existingVpc.ID, Deleted: true})
			if clearErr != nil {
				return nil, fmt.Errorf("unable to create VPC found on Site: failed to clear soft-delete timestamp for VPC, DB error: %w", clearErr)
			}
			return restored, nil
		}

		tenants, _, tenantErr := cdbm.NewTenantDAO(mv.dbSession).GetAll(
			ctx, tx, cdbm.TenantFilterInput{Orgs: []string{reportedVpc.Org}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil,
		)
		if tenantErr != nil {
			return nil, fmt.Errorf("unable to create VPC found on Site: failed to retrieve Tenant by organization, DB error: %w", tenantErr)
		}
		if len(tenants) == 0 {
			logger.Warn().Msgf("unable to create VPC found on Site: no Tenants were found for org: %s", reportedVpc.Org)
			return nil, nil
		}
		tenant := &tenants[0]

		nsgID := reportedVpc.NetworkSecurityGroupID
		nvllpID := reportedVpc.NVLinkLogicalPartitionID

		// Skip when referenced NSG/NVLink is missing or not owned by this tenant/site.
		if nsgID != nil {
			nsg, nsgErr := cdbm.NewNetworkSecurityGroupDAO(mv.dbSession).GetByID(ctx, tx, *nsgID, nil)
			if errors.Is(nsgErr, cdb.ErrDoesNotExist) {
				logger.Warn().Msgf("unable to create VPC found on Site: no Network Security Group was found for ID: %s", *nsgID)
				return nil, nil
			}
			if nsgErr != nil {
				return nil, fmt.Errorf("unable to create VPC found on Site: failed to retrieve Network Security Group by ID: %s, DB error: %w", *nsgID, nsgErr)
			}
			if nsg.TenantID != tenant.ID {
				logger.Warn().Msgf("unable to create VPC found on Site: Network Security Group differs in REST cache and Site record for Tenant %s", *nsgID)
				return nil, nil
			}
		}

		if nvllpID != nil {
			nvllp, nvLinkErr := cdbm.NewNVLinkLogicalPartitionDAO(mv.dbSession).GetByID(ctx, tx, *nvllpID, nil)
			if errors.Is(nvLinkErr, cdb.ErrDoesNotExist) {
				logger.Warn().Msgf("unable to create VPC found on Site: no NVLink Logical Partition was found for ID: %s", *nvllpID)
				return nil, nil
			}
			if nvLinkErr != nil {
				return nil, fmt.Errorf("unable to create VPC found on Site: failed to retrieve NVLink Logical Partition by ID: %s, DB error: %w", *nvllpID, nvLinkErr)
			}
			if nvllp.TenantID != tenant.ID {
				logger.Warn().Msgf("unable to create VPC found on Site: NVLink Logical Partition differs in REST cache and Site record for Tenant %s", *nvllpID)
				return nil, nil
			}
		}

		// If an active VPC already uses this name for the Tenant/Site, append a recovered suffix.
		nameConflictVpcs, _, nameErr := vpcDAO.GetAll(ctx, tx, cdbm.VpcFilterInput{
			Name: &reportedVpc.Name, TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{site.ID},
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if nameErr != nil {
			return nil, fmt.Errorf("unable to create VPC found on Site: failed to retrieve VPC by name, DB error: %w", nameErr)
		}
		if len(nameConflictVpcs) > 0 {
			reportedVpc.Name = fmt.Sprintf("%s-recovered-%s", reportedVpc.Name, vpcID.String()[:8])
		}

		readyMsg := "VPC was found on Site, Ready for use"
		createdBy := cdbm.User{}
		createdBy.ID = site.ID
		created, createErr := vpcDAO.Create(ctx, tx, cdbm.VpcCreateInput{
			ID:                                     &vpcID,
			Name:                                   reportedVpc.Name,
			Description:                            reportedVpc.Description,
			Org:                                    reportedVpc.Org,
			InfrastructureProviderID:               site.InfrastructureProviderID,
			TenantID:                               tenant.ID,
			SiteID:                                 site.ID,
			NVLinkLogicalPartitionID:               nvllpID,
			NetworkVirtualizationType:              reportedVpc.NetworkVirtualizationType,
			SlaacEnabled:                           reportedVpc.SlaacEnabled,
			RoutingProfile:                         reportedVpc.RoutingProfile,
			RoutingProfileOverrides:                reportedVpc.RoutingProfileOverrides,
			EffectiveRoutingProfile:                reportedVpc.EffectiveRoutingProfile,
			ControllerVpcID:                        &vpcID,
			ActiveVni:                              reportedVpc.ActiveVni,
			NetworkSecurityGroupID:                 nsgID,
			NetworkSecurityGroupPropagationDetails: propagationDetails,
			Labels:                                 reportedVpc.Labels,
			Status:                                 cdbm.VpcStatusReady,
			CreatedBy:                              createdBy,
			Vni:                                    reportedVpc.Vni,
		})
		if createErr != nil {
			return nil, fmt.Errorf("unable to create VPC found on Site: failed to create VPC, DB error: %w", createErr)
		}
		if _, statusErr := cdbm.NewStatusDetailDAO(mv.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: created.ID.String(), Status: cdbm.VpcStatusReady, Message: &readyMsg,
		}); statusErr != nil {
			return nil, fmt.Errorf("unable to create VPC found on Site: failed to create Status Detail, DB error: %w", statusErr)
		}
		return created, nil
	})
	if err != nil {
		logger.Warn().Err(err).Msg(err.Error())
		return nil
	}
	return vpc
}

// updateVpcStatusInDB is helper function to write VPC updates to DB
func (mv ManageVpc) updateVpcStatusInDB(ctx context.Context, tx *cdb.Tx, vpcID uuid.UUID, status *string, statusMessage *string) error {
	if status != nil {
		vpcDAO := cdbm.NewVpcDAO(mv.dbSession)

		_, err := vpcDAO.Update(ctx, tx, cdbm.VpcUpdateInput{VpcID: vpcID, Status: status})
		if err != nil {
			return err
		}

		statusDetailDAO := cdbm.NewStatusDetailDAO(mv.dbSession)
		_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: vpcID.String(), Status: *status, Message: statusMessage})
		if err != nil {
			return err
		}
	}
	return nil
}

// UpdateVpcMetadata is a Temporal activity that will trigger an update of an vpc's metadata
// if they are found out of sync with the cloud.
func (mv ManageVpc) UpdateVpcMetadata(ctx context.Context, siteID uuid.UUID, tc client.Client, vpcID uuid.UUID, controllerVpc *corev1.Vpc) error {
	logger := log.With().Str("Activity", "UpdateVpcMetadata").Str("Site ID", siteID.String()).Str("VPC ID", vpcID.String()).Logger()

	logger.Info().Msg("starting activity")

	vpcDAO := cdbm.NewVpcDAO(mv.dbSession)
	vpc, err := vpcDAO.GetByID(ctx, nil, vpcID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve VPC from DB by ID")
		return err
	}

	logger.Info().Msg("retrieved VPC from DB")

	description := ""
	if vpc.Description != nil {
		description = *vpc.Description
	}

	// Prepare the labels for the metadata of the nico call.
	labels := []*corev1.Label{}
	for k, v := range vpc.Labels {
		labels = append(labels, &corev1.Label{
			Key:   k,
			Value: &v,
		})
	}

	// Build an update request for vpc that needs a sync metadata and call UpdateVpc.
	workflowOptions := client.StartWorkflowOptions{
		ID:        "site-vpc-update-metadata-" + vpcID.String(),
		TaskQueue: queue.SiteTaskQueue,
	}

	// Prepare the config update request workflow object. NetworkSecurityGroupId is
	// intentionally omitted: this activity only syncs metadata fields.
	updateVpcRequest := &corev1.VpcUpdateRequest{
		Id: &corev1.VpcId{Value: vpc.ID.String()},
		Metadata: &corev1.Metadata{
			Name:        vpc.Name,
			Description: description,
			Labels:      labels,
		},
	}

	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, "UpdateVPC", updateVpcRequest)
	if err != nil {
		logger.Error().Err(err).Str("VPC ID", vpc.ID.String()).Msg("failed to trigger workflow to update VPC Metadata")
	} else {
		logger.Info().Str("Workflow ID", we.GetID()).Msg("triggered workflow to update VPC Metadata")
	}

	logger.Info().Msg("completed activity")

	return nil
}

// NewManageVpc returns a new ManageVpc activity
func NewManageVpc(dbSession *cdb.Session, siteClientPool *sc.ClientPool, tc client.Client) ManageVpc {
	return ManageVpc{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
		tc:             tc,
	}
}

// ManageVpcLifecycleMetrics is an activity wrapper for managing VPC lifecycle metrics
type ManageVpcLifecycleMetrics struct {
	dbSession            *cdb.Session
	statusTransitionTime *prometheus.GaugeVec
	siteNames            *cwm.SiteNameCache
}

// RecordVpcStatusTransitionMetrics is a Temporal activity that records duration of important status transitions for VPCs
func (mvlm ManageVpcLifecycleMetrics) RecordVpcStatusTransitionMetrics(ctx context.Context, siteID uuid.UUID, vpcLifecycleEvents []cwm.InventoryObjectLifecycleEvent) error {
	logger := log.With().Str("Activity", "RecordVpcStatusTransitionMetrics").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	siteName, err := mvlm.siteNames.Get(ctx, mvlm.dbSession, siteID)
	if err != nil {
		logger.Error().Err(err).Str("Site ID", siteID.String()).Msg("failed to retrieve Site from DB")
		return err
	}

	logger.Info().Int("EventCount", len(vpcLifecycleEvents)).Str("Site Name", siteName).Msg("processing vpc lifecycle events")

	// Get status details for each VPC
	sdDAO := cdbm.NewStatusDetailDAO(mvlm.dbSession)
	metricsRecorded := 0

	for _, event := range vpcLifecycleEvents {
		statusDetails, _, err := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{event.ObjectID.String()}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)})
		if err != nil {
			logger.Error().Err(err).Str("VPC ID", event.ObjectID.String()).Msg("failed to retrieve Status Details for VPC")
			return err
		}

		if event.Created != nil {
			// NOTE: VPC create operation is not tracked in this activity since it is created in synchronous manner and it should never arrive here
			logger.Warn().Str("VPC ID", event.ObjectID.String()).Msg("VPC create operation is not tracked in this activity since it is created in synchronous manner and it should never arrive here")
		} else if event.Deleted != nil {
			// DELETE event: Measure time from Deleting to actual deletion
			// Find the earliest Deleting status (iterate backwards since sorted DESC)
			var deletingStatusDetail *cdbm.StatusDetail
			for i := range slices.Backward(statusDetails) {
				sd := &statusDetails[i]
				if sd.Status == cdbm.VpcStatusDeleting {
					deletingStatusDetail = sd
					break
				}
			}

			if deletingStatusDetail != nil {
				// Calculate duration from Deleting status to deletion time
				duration := event.Deleted.Sub(deletingStatusDetail.Created)
				// Note: VPC doesn't have VpcStatusDeleted constant, so we use string "Deleted"
				mvlm.statusTransitionTime.WithLabelValues(siteName, siteID.String(), cwm.InventoryOperationTypeDelete, cdbm.VpcStatusDeleting, "Deleted").Set(duration.Seconds())
				metricsRecorded++
				logger.Info().
					Str("VPC ID", event.ObjectID.String()).
					Str("Operation", "DELETE").
					Float64("Duration Seconds", duration.Seconds()).
					Msg("recorded vpc lifecycle metric")
			} else {
				logger.Debug().
					Str("VPC ID", event.ObjectID.String()).
					Msg("skipped vpc DELETE metric")
			}
		}
	}

	logger.Info().Int("MetricsRecorded", metricsRecorded).Msg("completed activity")

	return nil
}

// NewManageVpcLifecycleMetrics returns a new ManageVpcLifecycleMetrics activity
func NewManageVpcLifecycleMetrics(reg prometheus.Registerer, dbSession *cdb.Session, namespace string) ManageVpcLifecycleMetrics {
	lifecycleMetrics := ManageVpcLifecycleMetrics{
		dbSession: dbSession,
		statusTransitionTime: prometheus.NewGaugeVec(
			prometheus.GaugeOpts{
				Namespace: namespace,
				Name:      "vpc_operation_latency_seconds",
				Help:      "Current latency of vpc operations",
			},
			[]string{"site", "site_id", "operation_type", "from_status", "to_status"}),

		siteNames: cwm.NewSiteNameCache(),
	}
	reg.MustRegister(lifecycleMetrics.statusTransitionTime)

	return lifecycleMetrics
}
