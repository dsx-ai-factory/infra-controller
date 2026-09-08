// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package subnet

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/netip"
	"slices"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/client"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	"github.com/prometheus/client_golang/prometheus"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

const (
	// DefaultReservedIPCount is the number of IP addresses to reserve in the subnet (usually the first and the last)
	DefaultReservedIPCount = 2
)

// ManageSubnet is an activity wrapper for managing Subnet lifecycle that allows
// injecting DB access
type ManageSubnet struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
	tc             client.Client
}

// Activity functions

// UpdateSubnetsInDB is a Temporal activity that takes a collection of Subnet/Network Segment data pushed by Site Agent and updates the DB
func (ms ManageSubnet) UpdateSubnetsInDB(ctx context.Context, siteID uuid.UUID, subnetInventory *corev1.SubnetInventory) ([]cwm.InventoryObjectLifecycleEvent, error) {
	logger := log.With().Str("Activity", "UpdateSubnetsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	// Initialize lifecycle events collector for metrics
	subnetLifecycleEvents := []cwm.InventoryObjectLifecycleEvent{}

	stDAO := cdbm.NewSiteDAO(ms.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received Subnet inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return nil, err
	}

	if subnetInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil, nil
	}

	subnetDAO := cdbm.NewSubnetDAO(ms.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(ms.dbSession)

	subnets, total, err := subnetDAO.GetAll(ctx, nil, cdbm.SubnetFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, []string{})
	if err != nil {
		logger.Error().Err(err).Msg("failed to get Subnets for Site from DB")
		return nil, err
	}

	if total == 0 {
		logger.Info().Msg("No Subnets found for Site")
	}

	// Construct a map of Controller Segment ID to Subnet
	existingSubnetIDMap := make(map[string]*cdbm.Subnet)
	existingSubnetCtrlIDMap := make(map[string]*cdbm.Subnet)

	for _, subnet := range subnets {
		foundSubnet := subnet
		existingSubnetIDMap[subnet.ID.String()] = &foundSubnet
		if subnet.ControllerNetworkSegmentID != nil {
			existingSubnetCtrlIDMap[subnet.ControllerNetworkSegmentID.String()] = &foundSubnet
		}
	}

	reportedSubnetIDMap := map[uuid.UUID]bool{}

	if subnetInventory.InventoryPage != nil {
		logger.Info().Msgf("Received Subnet inventory page: %d of %d, page size: %d, total count: %d",
			subnetInventory.InventoryPage.CurrentPage, subnetInventory.InventoryPage.TotalPages,
			subnetInventory.InventoryPage.PageSize, subnetInventory.InventoryPage.TotalItems)

		for _, strId := range subnetInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strId).Msg("failed to parse Subnet ID from inventory page")
				continue
			}
			reportedSubnetIDMap[id] = true
		}
	}

	// Iterate through Subnet Inventory and update DB
	for _, controllerSegment := range subnetInventory.Segments {
		if controllerSegment == nil || controllerSegment.GetId().GetValue() == "" {
			logger.Error().Msg("received Subnet inventory entry with missing controller ID, skipping")
			continue
		}

		controllerSegmentIDStr := controllerSegment.GetId().GetValue()
		slogger := logger.With().Str("Controller Segment ID", controllerSegmentIDStr).Logger()

		subnet, ok := existingSubnetCtrlIDMap[controllerSegmentIDStr]
		if !ok {
			// Check if the Subnet is found by ID (segment name == cloudSubnet.ID)
			subnet, ok = existingSubnetIDMap[controllerSegment.GetMetadata().GetName()]
			if ok {
				existingSubnetCtrlIDMap[controllerSegmentIDStr] = subnet
			}
		}

		if subnet == nil {
			if controllerSegment.GetConfig().GetSegmentType() != corev1.NetworkSegmentType_TENANT {
				continue
			}

			reportedStatus, _ := getControllerSubnetStatus(controllerSegment.GetStatus())
			if reportedStatus == cdbm.SubnetStatusDeleting || reportedStatus == cdbm.SubnetStatusDeleted {
				slogger.Info().Msgf("skipping create or undelete of Subnet from Site inventory: Site reports status %s", reportedStatus)
				continue
			}

			subnet = ms.createOrUpdateSubnetFromSite(ctx, site, controllerSegment)
			if subnet == nil {
				continue
			}

			// Keep in-memory maps in sync so later inventory entries and missing-on-Site detection see this Subnet.
			existingSubnetIDMap[subnet.ID.String()] = subnet
			if subnet.ControllerNetworkSegmentID != nil {
				existingSubnetCtrlIDMap[subnet.ControllerNetworkSegmentID.String()] = subnet
			}
			slogger.Info().Str("Subnet ID", subnet.ID.String()).Msg("created or undeleted Subnet from Site inventory")
		}

		reportedSubnetIDMap[subnet.ID] = true

		// Reset missing flag if necessary
		var isMissingOnSite *bool
		if subnet.IsMissingOnSite {
			isMissingOnSite = cwutil.GetPtr(false)
		}

		// Populate controller Subnet ID if necessary
		var controllerSegmentID *uuid.UUID
		if subnet.ControllerNetworkSegmentID == nil {
			ctrlID, serr := uuid.Parse(controllerSegment.Id.Value)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to parse Subnet Controller ID, not a valid UUID")
				continue
			}
			controllerSegmentID = &ctrlID
		}

		var mtu *int
		cfg := controllerSegment.GetConfig()
		if cfg != nil && cfg.Mtu != nil {
			mtuVal := int(*cfg.Mtu)
			mtu = &mtuVal
		}

		if mtu != nil || isMissingOnSite != nil || controllerSegmentID != nil {
			_, serr := subnetDAO.Update(ctx, nil, cdbm.SubnetUpdateInput{SubnetId: subnet.ID, ControllerNetworkSegmentID: controllerSegmentID, Mtu: mtu, IsMissingOnSite: cwutil.GetPtr(false)})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update MTU/missing on Site flag/controller Segment ID in DB")
				continue
			}
		}

		// Update Subnet in DB
		status, statusMessage := getControllerSubnetStatus(controllerSegment.GetStatus())

		// Preserve Deleting until the Subnet disappears from inventory and the
		// cleanup path releases IPAM and soft-deletes the DB row.
		if subnet.Status == cdbm.SubnetStatusDeleting {
			continue
		}

		// Check if most recent status detail is the same as the current status, otherwise create a new one
		updateStatusInDB := false
		if subnet.Status != status {
			// Status is different, create a new status detail
			updateStatusInDB = true
		} else {
			// Check if the latest status detail message is different from the current status message
			// Leave orderBy nil since the result is sorted by create timestamp by default
			latestsd, _, serr := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{subnet.ID.String()}}, cdbp.PageInput{Limit: cwutil.GetPtr(1)})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to retrieve latest Status Detail for Subnet")
			} else if len(latestsd) == 0 || latestsd[0].Message == nil || *latestsd[0].Message != statusMessage {
				updateStatusInDB = true
			}
		}

		if updateStatusInDB {
			serr := ms.updateSubnetStatusInDB(ctx, nil, subnet.ID, &status, &statusMessage)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update status and/or create Status Detail in DB")
			} else {
				// When subnet becomes Ready, record a creation lifecycle event; actual duration will be computed from StatusDetails
				if status == cdbm.SubnetStatusReady {
					slogger.Info().Str("To Status", status).Msg("recording subnet create lifecycle event")
					subnetLifecycleEvents = append(subnetLifecycleEvents, cwm.InventoryObjectLifecycleEvent{ObjectID: subnet.ID, Created: cwutil.GetPtr(time.Now())})
				}
			}
		}
	}

	// Process Subnets that were not found
	subnetsToDelete := []*cdbm.Subnet{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if subnetInventory.InventoryPage == nil || subnetInventory.InventoryPage.TotalPages == 0 || (subnetInventory.InventoryPage.CurrentPage == subnetInventory.InventoryPage.TotalPages) {
		for _, subnet := range existingSubnetIDMap {
			found := false

			_, found = reportedSubnetIDMap[subnet.ID]
			if !found && subnet.ControllerNetworkSegmentID != nil {
				// Additional check if controller Segment ID != Subnet ID
				_, found = reportedSubnetIDMap[*subnet.ControllerNetworkSegmentID]
			}

			if !found {
				// The Subnet was not found in the Subnet Inventory, so add it to list of Subnets to potentially terminate
				subnetsToDelete = append(subnetsToDelete, subnet)
			}
		}
	}

	// Loop through and remove controller Network Segment ID from Subnets that were not found
	for _, subnet := range subnetsToDelete {
		slogger := logger.With().Str("Subnet ID", subnet.ID.String()).Logger()

		// If the Subnet was already being deleted, we can proceed with removing it from the DB
		if subnet.Status == cdbm.SubnetStatusDeleting {
			// Retrieve Subnet with IPBlock
			curSubnet, serr := subnetDAO.GetByID(ctx, nil, subnet.ID, []string{cdbm.IPv4BlockRelationName})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to get Subnet from DB")
				continue
			}

			// The Subnet was being deleted, so delete it from DB
			tx, terr := cdb.BeginTx(ctx, ms.dbSession, &sql.TxOptions{})
			if terr != nil {
				slogger.Error().Err(terr).Msg("failed to start transaction")
				return subnetLifecycleEvents, terr
			}

			serr = ms.deleteSubnetFromDB(ctx, tx, curSubnet, logger)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to delete Subnet from DB")
				terr := tx.Rollback()
				if terr != nil {
					slogger.Error().Err(terr).Msg("failed to rollback transaction")
				}
			} else {
				err = tx.Commit()
				if err != nil {
					slogger.Error().Err(err).Msg("error committing Subnet delete transaction to DB")
				} else {
					// Add delete lifecycle event for metrics
					slogger.Info().Str("Subnet ID", curSubnet.ID.String()).Msg("recording subnet delete lifecycle event")
					subnetLifecycleEvents = append(subnetLifecycleEvents, cwm.InventoryObjectLifecycleEvent{ObjectID: curSubnet.ID, Deleted: cwutil.GetPtr(time.Now())})
				}
			}
		} else if subnet.ControllerNetworkSegmentID != nil {
			// Was this created within inventory receipt interval? If so, we may be processing an older inventory
			if site.IsTimeWithinStaleInventoryThreshold(subnet.Created) {
				continue
			}

			status := cdbm.SubnetStatusError
			statusMessage := "Subnet is missing on Site"

			// Leave orderBy as nil as the result is sorted by created timestamp by default
			if status == subnet.Status {
				latestsd, _, serr := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{subnet.ID.String()}}, cdbp.PageInput{Limit: cwutil.GetPtr(1)})
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to retrieve latest Status Detail for Subnet")
					continue
				}

				if len(latestsd) > 0 && latestsd[0].Message != nil && *latestsd[0].Message == statusMessage {
					continue
				}
			}

			// Set isMissingOnSite flag to true and update status, user can decide on deletion
			_, serr := subnetDAO.Update(ctx, nil, cdbm.SubnetUpdateInput{SubnetId: subnet.ID, IsMissingOnSite: cwutil.GetPtr(true)})
			if serr != nil {
				// Log error and continue
				slogger.Error().Err(serr).Msg("failed to set missing on Site flag in DB")
			}

			err = ms.updateSubnetStatusInDB(ctx, nil, subnet.ID, &status, &statusMessage)
			if err != nil {
				// Log error and continue
				slogger.Error().Err(serr).Msg("failed to update status and/or create Status Detail in DB")
			}
		}
	}

	return subnetLifecycleEvents, nil
}

// createOrUpdateSubnetFromSite creates a REST Subnet from Site inventory, or undeletes
// a matching soft-deleted row (and resets Status from Site inventory on undelete).
// Returns nil when skipped or on failure.
func (ms ManageSubnet) createOrUpdateSubnetFromSite(
	ctx context.Context,
	site *cdbm.Site,
	controllerSegment *corev1.NetworkSegment,
) *cdbm.Subnet {
	logger := log.With().
		Str("Activity", "UpdateSubnetsInDB").
		Str("Site ID", site.ID.String()).
		Str("Controller Segment ID", controllerSegment.GetId().GetValue()).
		Logger()

	// Get the Controller Segment ID from the Site inventory
	controllerSegmentID, err := uuid.Parse(controllerSegment.GetId().GetValue())
	if err != nil {
		logger.Warn().Msgf("unable to create Subnet found on Site: failed to parse Controller Segment ID, not a valid UUID %s", controllerSegment.GetId().GetValue())
		return nil
	}

	// Get the reported Subnet from the Site inventory
	reportedSubnet := new(cdbm.Subnet)
	reportedSubnet.FromProto(controllerSegment)
	if reportedSubnet.Name == "" {
		reportedSubnet.Name = fmt.Sprintf("recovered-%s", controllerSegmentID.String()[:8])
	}
	if reportedSubnet.IPv4Prefix == nil {
		logger.Warn().Msg("unable to create Subnet found on Site: Subnet on Site is reporting empty IPv4 Prefix")
		return nil
	}
	reportedPrefixCIDR := ipam.GetCidrForIPBlock(ctx, *reportedSubnet.IPv4Prefix, reportedSubnet.PrefixLength)
	reportedPrefix, err := netip.ParsePrefix(reportedPrefixCIDR)
	if err != nil {
		logger.Warn().Msgf("unable to create Subnet found on Site: failed to parse Prefix CIDR %s", reportedPrefixCIDR)
		return nil
	}
	// netip.ParsePrefix accepts host bits (e.g. 10.20.0.1/16). Reject those before any
	// IPAM mutation; otherwise the equal-length full-grant path can persist FullGrant
	// with no Subnet when a later soft-skip commits the transaction.
	maskedPrefixCIDR := reportedPrefix.Masked().String()
	if reportedPrefix.String() != maskedPrefixCIDR {
		logger.Warn().Msgf("unable to create Subnet found on Site: Prefix CIDR %s is not in canonical masked form %s", reportedPrefixCIDR, maskedPrefixCIDR)
		return nil
	}
	reportedPrefixAddress := reportedPrefix.Masked().Addr().String()
	reportedSubnet.IPv4Prefix = &reportedPrefixAddress
	reportedSubnet.PrefixLength = reportedPrefix.Bits()
	if reportedSubnet.VpcID == uuid.Nil {
		logger.Warn().Msg("unable to create Subnet found on Site: Subnet on Site is reporting empty VPC ID")
		return nil
	}
	parentVpcID := reportedSubnet.VpcID

	subnet, err := cdb.WithTxResult(ctx, ms.dbSession, func(tx *cdb.Tx) (*cdbm.Subnet, error) {
		subnetDAO := cdbm.NewSubnetDAO(ms.dbSession)
		vpcDAO := cdbm.NewVpcDAO(ms.dbSession)
		sdDAO := cdbm.NewStatusDetailDAO(ms.dbSession)

		// Parent VPC must already exist in REST; inventory VpcId is the Site-facing VPC ID.
		vpcMatches, _, vpcErr := vpcDAO.GetAll(ctx, tx, cdbm.VpcFilterInput{
			VpcIDs: []uuid.UUID{parentVpcID}, SiteIDs: []uuid.UUID{site.ID},
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if vpcErr != nil {
			return nil, fmt.Errorf("unable to create Subnet found on Site: failed to retrieve parent VPC by ID, DB error: %w", vpcErr)
		}
		if len(vpcMatches) == 0 {
			// The VPC inventory activity will create a missing parent VPC, so retry this
			// Subnet on the next inventory iteration.
			logger.Warn().Msgf("unable to create Subnet found on Site: no VPC was found for ID: %s", parentVpcID)
			return nil, nil
		}
		vpc := &vpcMatches[0]

		// Lookup by primary key globally (not scoped to site.ID). A same-UUID row under
		// another Site would otherwise be invisible here and Create would unique-constraint
		// fail every inventory cycle.
		matches, _, reloadErr := subnetDAO.GetAll(ctx, tx, cdbm.SubnetFilterInput{
			SubnetIDs:      []uuid.UUID{reportedSubnet.ID},
			IncludeDeleted: true,
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if reloadErr != nil {
			return nil, fmt.Errorf("unable to create Subnet found on Site: failed to retrieve Subnet by Controller Segment ID, DB error: %w", reloadErr)
		}

		// Non-nil from here on means this inventory entry is an undelete, not a create.
		var existingSubnet *cdbm.Subnet
		if len(matches) > 0 {
			existingSubnet = &matches[0]
		}

		// If the Subnet was found in the DB, check that it is valid before restoring it.
		if existingSubnet != nil {
			if existingSubnet.SiteID != site.ID {
				logger.Warn().Msgf("unable to create Subnet found on Site: Subnet ID already exists under a different Site for Subnet %s", controllerSegmentID)
				return nil, nil
			}
			if existingSubnet.Deleted == nil {
				return existingSubnet, nil
			}
			if existingSubnet.VpcID != vpc.ID {
				logger.Warn().Msgf("unable to create Subnet found on Site: VPC differs in REST cache and Site record for Subnet %s", controllerSegmentID)
				return nil, nil
			}
			if existingSubnet.Org != vpc.Org {
				logger.Warn().Msgf("unable to create Subnet found on Site: tenant organization differs in REST cache and Site record %s", vpc.Org)
				return nil, nil
			}
			// Clear restores the row as stored; do not acquire a Site CIDR that disagrees with
			// the cached Prefix/VpcID or DB and IPAM will permanently diverge.
			if existingSubnet.IPv4Prefix == nil {
				logger.Warn().Msgf("unable to create Subnet found on Site: stored IPv4 Prefix is missing for Subnet %s", controllerSegmentID)
				return nil, nil
			}
			existingPrefixCIDR := ipam.GetCidrForIPBlock(ctx, *existingSubnet.IPv4Prefix, existingSubnet.PrefixLength)
			existingPrefix, parseErr := netip.ParsePrefix(existingPrefixCIDR)
			if parseErr != nil || existingPrefix.Masked().String() != maskedPrefixCIDR {
				logger.Warn().Msgf("unable to create Subnet found on Site: Prefix differs in REST cache and Site record for Subnet %s", controllerSegmentID)
				return nil, nil
			}
			// Deleted records when the delete happened, so a delete newer than the interval can
			// postdate this inventory. Undeleting then would revive a Subnet the snapshot
			// never saw removed. Skip before the IPAM work below rather than after, so there is
			// no allocation to unwind. A later inventory undeletes it if the Site still reports it.
			if site.IsTimeWithinStaleInventoryThreshold(*existingSubnet.Deleted) {
				logger.Info().Msgf("not undeleting Subnet %s yet because it was deleted more recently than the inventory interval", controllerSegmentID)
				return nil, nil
			}
		}

		// If the Subnet is being undeleted, use its stored IPv4 Block. Otherwise
		// find the most specific Ready tenant IPv4 Block that contains its Prefix.
		ipBlockDAO := cdbm.NewIPBlockDAO(ms.dbSession)
		var ipBlock *cdbm.IPBlock
		if existingSubnet != nil {
			if existingSubnet.IPv4BlockID == nil {
				logger.Warn().Msgf("unable to create Subnet found on Site: stored IPv4 Block ID is missing for Subnet %s", existingSubnet.ID)
				return nil, nil
			}

			ipBlock, reloadErr = ipBlockDAO.GetByID(ctx, tx, *existingSubnet.IPv4BlockID, nil)
			if reloadErr != nil {
				if errors.Is(reloadErr, cdb.ErrDoesNotExist) {
					logger.Warn().Msgf("unable to create Subnet found on Site: stored IPv4 Block %s was not found for Subnet %s", existingSubnet.IPv4BlockID, existingSubnet.ID)
					return nil, nil
				}
				return nil, fmt.Errorf("unable to create Subnet found on Site: failed to retrieve stored IPv4 Block, DB error: %w", reloadErr)
			}
			if ipBlock.SiteID != site.ID {
				logger.Warn().Msgf("unable to create Subnet found on Site: stored IPv4 Block belongs to a different Site for Subnet %s", existingSubnet.ID)
				return nil, nil
			}
			if ipBlock.TenantID == nil || *ipBlock.TenantID != vpc.TenantID {
				logger.Warn().Msgf("unable to create Subnet found on Site: stored IPv4 Block belongs to a different Tenant for Subnet %s", existingSubnet.ID)
				return nil, nil
			}
			if !ipBlock.ContainsPrefix(reportedPrefix) {
				logger.Warn().Msgf("unable to create Subnet found on Site: stored IPv4 Block does not contain Prefix %s for Subnet %s", reportedPrefix, existingSubnet.ID)
				return nil, nil
			}
		} else {
			ipBlocks, _, ipBlockErr := ipBlockDAO.GetAll(ctx, tx, cdbm.IPBlockFilterInput{
				SiteIDs:   []uuid.UUID{site.ID},
				TenantIDs: []uuid.UUID{vpc.TenantID},
				Statuses:  []string{cdbm.IPBlockStatusReady},
			}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
			if ipBlockErr != nil {
				return nil, fmt.Errorf("unable to create Subnet found on Site: failed to retrieve IPv4 Blocks, DB error: %w", ipBlockErr)
			}

			for i := range ipBlocks {
				candidateIPBlock := &ipBlocks[i]
				if candidateIPBlock.FullGrant || !candidateIPBlock.ContainsPrefix(reportedPrefix) {
					continue
				}
				if ipBlock == nil || candidateIPBlock.PrefixLength > ipBlock.PrefixLength {
					ipBlock = candidateIPBlock
				}
			}
			if ipBlock == nil {
				logger.Warn().Msgf("unable to create Subnet found on Site: no containing IPv4 Block was found for Prefix: %s", reportedPrefix)
				return nil, nil
			}
		}

		// Claim the exact Site-reported CIDR in REST IPAM while holding the same
		// tenant/IPBlock lock used by the normal REST create path.
		lockErr := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(fmt.Sprintf("%s-%s", vpc.TenantID.String(), ipBlock.ID.String())), false)
		if lockErr != nil {
			return nil, fmt.Errorf("unable to create Subnet found on Site: failed to acquire advisory lock on IPv4 Block, DB error: %w", lockErr)
		}

		// Refresh FullGrant under the lock because the candidate scan read it before
		// the lock and a concurrent create may have changed it.
		freshIPBlock, reloadIPBlockErr := cdbm.NewIPBlockDAO(ms.dbSession).GetByID(ctx, tx, ipBlock.ID, nil)
		if reloadIPBlockErr != nil {
			return nil, fmt.Errorf("unable to create Subnet found on Site: failed to reload IPv4 Block under advisory lock, DB error: %w", reloadIPBlockErr)
		}
		ipBlock = freshIPBlock
		if ipBlock.Status != cdbm.IPBlockStatusReady {
			logger.Warn().Msgf("unable to create Subnet found on Site: IPv4 Block %s is no longer Ready", ipBlock.ID)
			return nil, nil
		}
		if ipBlock.FullGrant {
			logger.Warn().Msgf("unable to create Subnet found on Site: IPv4 Block %s was fully granted concurrently", ipBlock.ID)
			return nil, nil
		}

		ipamStorage := ipam.NewIpamStorage(ms.dbSession.DB, tx.GetBunTx())
		reportedPrefixLength := reportedPrefix.Bits()

		var allocateErr error
		if ipBlock.PrefixLength == reportedPrefixLength {
			_, allocateErr = ipam.CreateChildIpamEntryForIPBlock(
				ctx, tx, ms.dbSession, ipamStorage, ipBlock, reportedPrefixLength,
			)
		} else {
			_, allocateErr = ipam.AcquireSpecificChildIpamEntryForIPBlock(
				ctx, tx, ms.dbSession, ipamStorage, ipBlock, maskedPrefixCIDR,
			)
		}
		if allocateErr != nil {
			return nil, fmt.Errorf(
				"unable to create Subnet found on Site: failed to create IPAM entry for Subnet: %w",
				allocateErr,
			)
		}

		// If the Subnet was soft-deleted, undelete it and reset Status from Site inventory.
		if existingSubnet != nil {
			restored, clearErr := subnetDAO.Clear(ctx, tx, cdbm.SubnetClearInput{SubnetId: existingSubnet.ID, Deleted: true})
			if clearErr != nil {
				return nil, fmt.Errorf("unable to create Subnet found on Site: failed to clear soft-delete timestamp for Subnet, DB error: %w", clearErr)
			}
			status, statusMessage := getControllerSubnetStatus(controllerSegment.GetStatus())
			updated, updateErr := subnetDAO.Update(ctx, tx, cdbm.SubnetUpdateInput{
				SubnetId: restored.ID,
				Status:   &status,
			})
			if updateErr != nil {
				return nil, fmt.Errorf("unable to create Subnet found on Site: failed to update Subnet status after undelete, DB error: %w", updateErr)
			}
			_, statusErr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
				EntityID: updated.ID.String(), Status: status, Message: &statusMessage,
			})
			if statusErr != nil {
				return nil, fmt.Errorf("unable to create Subnet found on Site: failed to create Status Detail after undelete, DB error: %w", statusErr)
			}
			return updated, nil
		}

		// If an active Subnet already uses this name for the Tenant/Site, append a recovered suffix.
		nameConflictSubnets, _, nameErr := subnetDAO.GetAll(ctx, tx, cdbm.SubnetFilterInput{
			Names: []string{reportedSubnet.Name}, TenantIDs: []uuid.UUID{vpc.TenantID}, SiteIDs: []uuid.UUID{site.ID},
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if nameErr != nil {
			return nil, fmt.Errorf("unable to create Subnet found on Site: failed to retrieve Subnet by name, DB error: %w", nameErr)
		}
		if len(nameConflictSubnets) > 0 {
			reportedSubnet.Name = fmt.Sprintf("%s-recovered-%s", reportedSubnet.Name, reportedSubnet.ID.String()[:8])
		}

		readyMsg := "Subnet was found on Site, Ready for use"
		created, createErr := subnetDAO.Create(ctx, tx, cdbm.SubnetCreateInput{
			SubnetID:                   &controllerSegmentID,
			Name:                       reportedSubnet.Name,
			Description:                reportedSubnet.Description,
			Org:                        vpc.Org,
			SiteID:                     site.ID,
			VpcID:                      vpc.ID,
			DomainID:                   reportedSubnet.DomainID,
			TenantID:                   vpc.TenantID,
			ControllerNetworkSegmentID: &controllerSegmentID,
			RoutingType:                &ipBlock.RoutingType,
			IPv4Prefix:                 reportedSubnet.IPv4Prefix,
			IPv4Gateway:                reportedSubnet.IPv4Gateway,
			IPv4BlockID:                &ipBlock.ID,
			PrefixLength:               reportedPrefixLength,
			Mtu:                        reportedSubnet.MTU,
			Status:                     cdbm.SubnetStatusReady,
			CreatedBy:                  vpc.CreatedBy,
		})
		if createErr != nil {
			return nil, fmt.Errorf("unable to create Subnet found on Site: failed to create Subnet, DB error: %w", createErr)
		}
		_, statusErr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: created.ID.String(), Status: cdbm.SubnetStatusReady, Message: &readyMsg,
		})
		if statusErr != nil {
			return nil, fmt.Errorf("unable to create Subnet found on Site: failed to create Status Detail, DB error: %w", statusErr)
		}
		return created, nil
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to recover Subnet from Site inventory")
		return nil
	}
	return subnet
}

// updateSubnetStatusInDB is helper function to write Subnet status updates to DB
func (ms ManageSubnet) updateSubnetStatusInDB(ctx context.Context, tx *cdb.Tx, subnetID uuid.UUID, status *string, statusMessage *string) error {
	if status != nil {
		subnetDAO := cdbm.NewSubnetDAO(ms.dbSession)

		_, err := subnetDAO.Update(ctx, tx, cdbm.SubnetUpdateInput{SubnetId: subnetID, Status: status})
		if err != nil {
			return err
		}

		statusDetailDAO := cdbm.NewStatusDetailDAO(ms.dbSession)
		_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: subnetID.String(), Status: *status, Message: statusMessage})
		if err != nil {
			return err
		}
	}
	return nil
}

// deleteSubnetFromDB is helper function to delete Subnet from DB
func (ms ManageSubnet) deleteSubnetFromDB(ctx context.Context, tx *cdb.Tx, subnet *cdbm.Subnet, logger zerolog.Logger) error {
	// Acquire an advisory lock on the parent IP block ID on which there could be contention
	// this lock is released when the transaction commits or rollsback
	err := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(subnet.IPv4BlockID.String()), false)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to acquire advisory lock on IP Block")
		terr := tx.Rollback()
		if terr != nil {
			logger.Error().Err(terr).Msg("failed to rollback transaction")
		}
		return err
	}
	logger.Info().Msg("acquired advisory lock on Subnet's IP Block")

	// Delete IPAM entry for this subnet
	ipamStorage := ipam.NewIpamStorage(ms.dbSession.DB, tx.GetBunTx())
	childCidr := ipam.GetCidrForIPBlock(ctx, *subnet.IPv4Prefix, subnet.PrefixLength)
	err = ipam.DeleteChildIpamEntryFromCidr(ctx, tx, ms.dbSession, ipamStorage, subnet.IPv4Block, childCidr)
	if err != nil {
		logger.Error().Err(err).Msg("failed to delete ipam record for Subnet")
		terr := tx.Rollback()
		if terr != nil {
			logger.Error().Err(terr).Msg("failed to rollback transaction")
		}
		return err
	}
	logger.Info().Msg("delete Subnet IPAM entry")

	// Soft-delete subnet
	subnetDAO := cdbm.NewSubnetDAO(ms.dbSession)

	err = subnetDAO.Delete(ctx, tx, subnet.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to delete Subnet from DB")
		terr := tx.Rollback()
		if terr != nil {
			logger.Error().Err(terr).Msg("failed to rollback transaction")
		}
		return err
	}

	return nil
}

// getControllerSubnetStatus maps Controller Network Segment tenant state into REST status and status-detail text.
func getControllerSubnetStatus(status *corev1.NetworkSegmentStatus) (string, string) {
	// Older Controller builds did not report status; inventory presence meant ready.
	if status == nil {
		return cdbm.SubnetStatusReady, "Subnet is ready for use"
	}

	switch status.GetTenantState() {
	case corev1.TenantState_PROVISIONING:
		return cdbm.SubnetStatusProvisioning, "Subnet is being provisioned on Site"
	case corev1.TenantState_READY:
		return cdbm.SubnetStatusReady, "Subnet is ready for use"
	case corev1.TenantState_CONFIGURING:
		return cdbm.SubnetStatusProvisioning, "Subnet is being configured on Site"
	case corev1.TenantState_TERMINATING:
		return cdbm.SubnetStatusDeleting, "Subnet is being deleted on Site"
	case corev1.TenantState_TERMINATED:
		return cdbm.SubnetStatusDeleted, "Subnet has been deleted on Site"
	case corev1.TenantState_FAILED:
		return cdbm.SubnetStatusError, "Subnet is in error state"
	default:
		return cdbm.SubnetStatusError, "Subnet status is unknown"
	}
}

// NewManageSubnet returns a new ManageSubnet activity
func NewManageSubnet(dbSession *cdb.Session, siteClientPool *sc.ClientPool, tc client.Client) ManageSubnet {
	return ManageSubnet{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
		tc:             tc,
	}
}

// ManageSubnetLifecycleMetrics is an activity wrapper for managing Subnet lifecycle metrics
type ManageSubnetLifecycleMetrics struct {
	dbSession            *cdb.Session
	statusTransitionTime *prometheus.GaugeVec
	siteNames            *cwm.SiteNameCache
}

// RecordSubnetStatusTransitionMetrics is a Temporal activity that records duration of important status transitions for Subnets
func (mslm ManageSubnetLifecycleMetrics) RecordSubnetStatusTransitionMetrics(ctx context.Context, siteID uuid.UUID, subnetLifecycleEvents []cwm.InventoryObjectLifecycleEvent) error {
	logger := log.With().Str("Activity", "RecordSubnetStatusTransitionMetrics").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	siteName, err := mslm.siteNames.Get(ctx, mslm.dbSession, siteID)
	if err != nil {
		logger.Error().Err(err).Str("Site ID", siteID.String()).Msg("failed to retrieve Site from DB")
		return err
	}

	logger.Info().Int("EventCount", len(subnetLifecycleEvents)).Str("Site Name", siteName).Msg("processing subnet lifecycle events")

	// Get status details for each Subnet in events
	sdDAO := cdbm.NewStatusDetailDAO(mslm.dbSession)
	metricsRecorded := 0

	for _, event := range subnetLifecycleEvents {
		statusDetails, _, err := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{event.ObjectID.String()}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)})
		if err != nil {
			logger.Error().Err(err).Str("Subnet ID", event.ObjectID.String()).Msg("failed to retrieve Status Details for Subnet")
			return err
		}

		if event.Created != nil {
			// CREATE event: Measure time from earliest Pending to Ready
			// Requirements:
			// 1. Must have exactly one Ready status (ensures clean transition)
			// 2. Find the earliest Pending status to calculate duration from
			var readySD *cdbm.StatusDetail
			var pendingSD *cdbm.StatusDetail
			readyStatusCount := 0

			for i := range statusDetails {
				if statusDetails[i].Status == cdbm.SubnetStatusReady {
					readyStatusCount++
					// Early exit if multiple Ready statuses found - indicates abnormal state
					if readyStatusCount > 1 {
						break
					}
					readySD = &statusDetails[i]
				} else if statusDetails[i].Status == cdbm.SubnetStatusPending {
					// Find the earliest Pending status (statusDetails sorted by Created DESC)
					pendingSD = &statusDetails[i]
				}
			}

			// Only emit metric if we have exactly 1 Ready and at least 1 Pending
			if readySD != nil && pendingSD != nil && readyStatusCount == 1 {
				dur := readySD.Created.Sub(pendingSD.Created)
				mslm.statusTransitionTime.WithLabelValues(siteName, siteID.String(), cwm.InventoryOperationTypeCreate, cdbm.SubnetStatusPending, cdbm.SubnetStatusReady).Set(dur.Seconds())
				metricsRecorded++
				logger.Info().
					Str("Subnet ID", event.ObjectID.String()).
					Str("Operation", "CREATE").
					Float64("Duration Seconds", dur.Seconds()).
					Msg("recorded subnet lifecycle metric")
			} else {
				logger.Debug().
					Str("Subnet ID", event.ObjectID.String()).
					Msg("skipped subnet CREATE metric")
			}
		} else if event.Deleted != nil {
			// DELETE event: Measure time from Deleting to actual deletion
			// Find the earliest Deleting status (iterate backwards since sorted DESC)
			var deletingSD *cdbm.StatusDetail
			for i := range slices.Backward(statusDetails) {
				if statusDetails[i].Status == cdbm.SubnetStatusDeleting {
					deletingSD = &statusDetails[i]
					break
				}
			}

			if deletingSD != nil {
				// Calculate duration from Deleting status to deletion time
				dur := event.Deleted.Sub(deletingSD.Created)
				mslm.statusTransitionTime.WithLabelValues(siteName, siteID.String(), cwm.InventoryOperationTypeDelete, cdbm.SubnetStatusDeleting, cdbm.SubnetStatusDeleted).Set(dur.Seconds())
				metricsRecorded++
				logger.Info().
					Str("Subnet ID", event.ObjectID.String()).
					Str("Operation", "DELETE").
					Float64("Duration Seconds", dur.Seconds()).
					Msg("recorded subnet lifecycle metric")
			} else {
				logger.Debug().
					Str("Subnet ID", event.ObjectID.String()).
					Msg("skipped subnet DELETE metric")
			}
		}
	}

	logger.Info().Int("MetricsRecorded", metricsRecorded).Msg("completed activity")
	return nil
}

// NewManageSubnetLifecycleMetrics returns a new ManageSubnetLifecycleMetrics activity
func NewManageSubnetLifecycleMetrics(reg prometheus.Registerer, dbSession *cdb.Session, namespace string) ManageSubnetLifecycleMetrics {
	lifecycleMetrics := ManageSubnetLifecycleMetrics{
		dbSession: dbSession,
		statusTransitionTime: prometheus.NewGaugeVec(
			prometheus.GaugeOpts{
				Namespace: namespace,
				Name:      "subnet_operation_latency_seconds",
				Help:      "Current latency of subnet operations",
			},
			[]string{"site", "site_id", "operation_type", "from_status", "to_status"}),

		siteNames: cwm.NewSiteNameCache(),
	}
	reg.MustRegister(lifecycleMetrics.statusTransitionTime)

	return lifecycleMetrics
}
