// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcprefix

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/netip"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ManageVpcPrefix is an activity wrapper for managing VPC Prefix lifecycle that allows
// injecting DB access
type ManageVpcPrefix struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions
// UpdateVpcPrefixesInDB is a Temporal activity that takes a collection of VPC Prefix data pushed by Site Agent and updates the DB
func (mvp ManageVpcPrefix) UpdateVpcPrefixesInDB(ctx context.Context, siteID uuid.UUID, vpcPrefixInventory *corev1.VpcPrefixInventory) error {
	logger := log.With().Str("Activity", "UpdateVpcPrefixesInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	stDAO := cdbm.NewSiteDAO(mvp.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received VPC Prefix inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	if vpcPrefixInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(mvp.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(mvp.dbSession)

	existingVpcPrefixes, _, err := vpcPrefixDAO.GetAll(ctx, nil, cdbm.VpcPrefixFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get VPC Prefixes for Site from DB")
		return err
	}

	// Construct a map of Controller VPC Prefix ID to VPC Prefix
	existingVpcPrefixIDMap := make(map[string]*cdbm.VpcPrefix)
	for _, vpcPrefix := range existingVpcPrefixes {
		curVpcPrefix := vpcPrefix
		existingVpcPrefixIDMap[vpcPrefix.ID.String()] = &curVpcPrefix
	}

	reportedVpcPrefixIDMap := map[uuid.UUID]bool{}

	if vpcPrefixInventory.InventoryPage != nil {
		logger.Info().Msgf("Received VPC Prefix inventory page: %d of %d, page size: %d, total count: %d",
			vpcPrefixInventory.InventoryPage.CurrentPage, vpcPrefixInventory.InventoryPage.TotalPages,
			vpcPrefixInventory.InventoryPage.PageSize, vpcPrefixInventory.InventoryPage.TotalItems)

		for _, strId := range vpcPrefixInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strId).Msg("failed to parse VPC Prefix ID from inventory page")
				continue
			}
			reportedVpcPrefixIDMap[id] = true
		}
	}

	// Iterate through VpcPrefix Inventory and update DB
	for _, controllerVpcPrefix := range vpcPrefixInventory.VpcPrefixes {
		if controllerVpcPrefix == nil || controllerVpcPrefix.GetId().GetValue() == "" {
			logger.Error().Msg("received VPC Prefix inventory entry with missing controller ID, skipping")
			continue
		}

		controllerVpcPrefixID := controllerVpcPrefix.GetId().GetValue()
		slogger := logger.With().Str("VPC Prefix Controller ID", controllerVpcPrefixID).Logger()
		vpcPrefix := existingVpcPrefixIDMap[controllerVpcPrefixID]

		// No active REST row for this inventory VPC Prefix: create one or undelete a soft-deleted match,
		// then fall through so the main inventory loop applies Site-reported status updates.
		if vpcPrefix == nil {
			// Inventory pushes are unordered Temporal workflows. A stale snapshot can still list a
			// just-deleted prefix as TERMINATING/TERMINATED (or even READY). Never create/undelete
			// from a terminal Site status, since that re-claims IPAM and resurrects a user delete.
			reportedStatus, _ := getControllerVpcPrefixStatus(controllerVpcPrefix.GetStatus())
			if reportedStatus == cdbm.VpcPrefixStatusDeleting || reportedStatus == cdbm.VpcPrefixStatusDeleted {
				slogger.Info().Msgf("skipping create or undelete of VPC Prefix from Site inventory: Site reports status %s", reportedStatus)
				continue
			}

			vpcPrefix = mvp.createOrUpdateVpcPrefixFromSite(ctx, site, controllerVpcPrefix)
			if vpcPrefix == nil {
				continue
			}

			// Keep in-memory maps in sync so later inventory entries and missing-on-Site detection see this VPC Prefix.
			existingVpcPrefixIDMap[vpcPrefix.ID.String()] = vpcPrefix
			logger.Info().Str("VPC Prefix ID", vpcPrefix.ID.String()).Msg("created or undeleted VPC Prefix from Site inventory")
		}

		reportedVpcPrefixIDMap[vpcPrefix.ID] = true

		// Reset missing flag if necessary
		var isMissingOnSite *bool
		if vpcPrefix.IsMissingOnSite {
			isMissingOnSite = cwutil.GetPtr(false)
		}

		if isMissingOnSite != nil {
			_, serr := vpcPrefixDAO.Update(ctx, nil, cdbm.VpcPrefixUpdateInput{VpcPrefixID: vpcPrefix.ID, IsMissingOnSite: isMissingOnSite})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update missing on Site flag/controller VPC Prefix ID in DB")
				continue
			}
		}

		// Update local status from the Controller tenant state reported in inventory.
		status, statusMessage := getControllerVpcPrefixStatus(controllerVpcPrefix.GetStatus())
		if vpcPrefix.Status == cdbm.VpcPrefixStatusDeleting && status != cdbm.VpcPrefixStatusDeleting && status != cdbm.VpcPrefixStatusDeleted {
			continue
		}

		if vpcPrefix.Status != status {
			err = mvp.updateVpcPrefixStatusInDB(ctx, nil, vpcPrefix.ID, &status, &statusMessage)
			if err != nil {
				slogger.Error().Err(err).Msg("failed to update VPC Prefix status detail in DB")
			}
		} else {
			latestsd, _, serr := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{vpcPrefix.ID.String()}}, cdbp.PageInput{Limit: cwutil.GetPtr(1)})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to retrieve latest Status Detail for VPC Prefix")
			} else if len(latestsd) == 0 || latestsd[0].Message == nil || *latestsd[0].Message != statusMessage {
				err = mvp.updateVpcPrefixStatusInDB(ctx, nil, vpcPrefix.ID, &status, &statusMessage)
				if err != nil {
					slogger.Error().Err(err).Msg("failed to update VPC Prefix status detail in DB")
				}
			}
		}

	}

	// Populate list of VpcPrefixes that were not found
	vpcPrefixesToDelete := []*cdbm.VpcPrefix{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if vpcPrefixInventory.InventoryPage == nil || vpcPrefixInventory.InventoryPage.TotalPages == 0 || (vpcPrefixInventory.InventoryPage.CurrentPage == vpcPrefixInventory.InventoryPage.TotalPages) {
		for _, vpcPrefix := range existingVpcPrefixIDMap {
			found := false

			_, found = reportedVpcPrefixIDMap[vpcPrefix.ID]
			if !found {
				// The VpcPrefix was not found in the VpcPrefix Inventory, so add it to list of VpcPrefixes to potentially terminate
				vpcPrefixesToDelete = append(vpcPrefixesToDelete, vpcPrefix)
			}
		}
	}

	// Loop through VpcPrefixes for deletion
	for _, vpcPrefix := range vpcPrefixesToDelete {
		slogger := logger.With().Str("VPC Prefix ID", vpcPrefix.ID.String()).Logger()

		// If the VpcPrefix was already deleting or deleted, we can remove it from the DB.
		if vpcPrefix.Status == cdbm.VpcPrefixStatusDeleting || vpcPrefix.Status == cdbm.VpcPrefixStatusDeleted {
			// Retrieve Subnet with IPBlock
			curVpcPrefix, serr := vpcPrefixDAO.GetByID(ctx, nil, vpcPrefix.ID, []string{cdbm.IPBlockRelationName})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to get VPC Prefix from DB")
				continue
			}

			// The Subnet was being deleted, so delete it from DB
			tx, terr := cdb.BeginTx(ctx, mvp.dbSession, &sql.TxOptions{})
			if terr != nil {
				slogger.Error().Err(terr).Msg("failed to start transaction")
				return terr
			}

			serr = mvp.deleteVpcPrefixFromDB(ctx, tx, curVpcPrefix, logger)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to delete VPC Prefix from DB")
				terr := tx.Rollback()
				if terr != nil {
					slogger.Error().Err(terr).Msg("failed to rollback transaction")
				}
			} else {
				err = tx.Commit()
				if err != nil {
					slogger.Error().Err(err).Msg("error committing VPC Prefix delete transaction to DB")
				}
			}
		} else {
			// Was this created within inventory receipt interval? If so, we may be processing an older inventory
			if site.IsTimeWithinStaleInventoryThreshold(vpcPrefix.Created) {
				continue
			}

			// Set isMissingOnSite flag to true and update status, user can decide on deletion
			_, serr := vpcPrefixDAO.Update(ctx, nil, cdbm.VpcPrefixUpdateInput{VpcPrefixID: vpcPrefix.ID, IsMissingOnSite: cwutil.GetPtr(true)})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to set missing on Site flag in DB")
				continue
			}

			serr = mvp.updateVpcPrefixStatusInDB(ctx, nil, vpcPrefix.ID, cwutil.GetPtr(cdbm.VpcPrefixStatusError), cwutil.GetPtr("VPC Prefix is missing on Site"))
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update VPC Prefix status detail in DB")
			}
		}
	}

	return nil
}

// createOrUpdateVpcPrefixFromSite creates a REST VPC Prefix from Site inventory, or undeletes
// a matching soft-deleted row (and resets Status from Site inventory on undelete).
// Returns nil when skipped or on failure.
func (mvp ManageVpcPrefix) createOrUpdateVpcPrefixFromSite(
	ctx context.Context,
	site *cdbm.Site,
	controllerVpcPrefix *corev1.VpcPrefix,
) *cdbm.VpcPrefix {
	logger := log.With().
		Str("Activity", "UpdateVpcPrefixesInDB").
		Str("Site ID", site.ID.String()).
		Str("VPC Prefix Controller ID", controllerVpcPrefix.GetId().GetValue()).
		Logger()

	// Get the controller VPC Prefix ID from the Site inventory
	controllerVpcPrefixID, err := uuid.Parse(controllerVpcPrefix.GetId().GetValue())
	if err != nil {
		logger.Warn().Msgf("unable to create VPC Prefix found on Site: failed to parse VPC Prefix Controller ID, not a valid UUID %s", controllerVpcPrefix.GetId().GetValue())
		return nil
	}

	// Get the reported VPC Prefix from the Site inventory
	reportedVpcPrefix := new(cdbm.VpcPrefix)
	reportedVpcPrefix.FromProto(controllerVpcPrefix)
	if reportedVpcPrefix.Name == "" {
		reportedVpcPrefix.Name = fmt.Sprintf("recovered-%s", controllerVpcPrefixID.String()[:8])
	}
	if reportedVpcPrefix.Prefix == "" {
		logger.Warn().Msg("unable to create VPC Prefix found on Site: VPC Prefix on Site is reporting empty Prefix")
		return nil
	}
	reportedPrefix, err := netip.ParsePrefix(reportedVpcPrefix.Prefix)
	if err != nil {
		logger.Warn().Msgf("unable to create VPC Prefix found on Site: failed to parse Prefix CIDR %s", reportedVpcPrefix.Prefix)
		return nil
	}
	// netip.ParsePrefix accepts host bits (e.g. 10.20.0.1/16). Reject those before any
	// IPAM mutation; otherwise the equal-length full-grant path can persist FullGrant
	// with no VpcPrefix when a later soft-skip commits the transaction.
	maskedPrefixCIDR := reportedPrefix.Masked().String()
	if reportedPrefix.String() != maskedPrefixCIDR {
		logger.Warn().Msgf("unable to create VPC Prefix found on Site: Prefix CIDR %s is not in canonical masked form %s", reportedVpcPrefix.Prefix, maskedPrefixCIDR)
		return nil
	}
	reportedVpcPrefix.Prefix = maskedPrefixCIDR
	if reportedVpcPrefix.VpcID == uuid.Nil {
		logger.Warn().Msg("unable to create VPC Prefix found on Site: VPC Prefix on Site is reporting empty VPC ID")
		return nil
	}
	parentVpcID := reportedVpcPrefix.VpcID

	vpcPrefix, err := cdb.WithTxResult(ctx, mvp.dbSession, func(tx *cdb.Tx) (*cdbm.VpcPrefix, error) {
		vpcPrefixDAO := cdbm.NewVpcPrefixDAO(mvp.dbSession)
		vpcDAO := cdbm.NewVpcDAO(mvp.dbSession)
		sdDAO := cdbm.NewStatusDetailDAO(mvp.dbSession)

		// Parent VPC must already exist in REST; inventory VpcId is the site-facing VPC ID.
		vpcMatches, _, vpcErr := vpcDAO.GetAll(ctx, tx, cdbm.VpcFilterInput{
			VpcIDs: []uuid.UUID{parentVpcID}, SiteIDs: []uuid.UUID{site.ID},
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if vpcErr != nil {
			return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to retrieve parent VPC by ID, DB error: %w", vpcErr)
		}
		if len(vpcMatches) == 0 {
			// Even if this happens, the VPC will be created based on the createOrUpdateVpcFromSite function in the vpc activity
			// so we are just returning nil and next inventory iteration VPC will be created in the vpc activity
			logger.Warn().Msgf("unable to create VPC Prefix found on Site: no VPC was found for ID: %s", parentVpcID)
			return nil, nil
		}
		vpc := &vpcMatches[0]

		// Lookup by primary key globally (not scoped to site.ID). A same-UUID row under
		// another site would otherwise be invisible here and Create would unique-constraint
		// fail every inventory cycle.
		matches, _, reloadErr := vpcPrefixDAO.GetAll(ctx, tx, cdbm.VpcPrefixFilterInput{
			VpcPrefixIDs:   []uuid.UUID{reportedVpcPrefix.ID},
			IncludeDeleted: true,
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if reloadErr != nil {
			return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to retrieve VPC Prefix by controller ID, DB error: %w", reloadErr)
		}

		// Non-nil from here on means this inventory entry is an undelete, not a create.
		var existingVpcPrefix *cdbm.VpcPrefix
		if len(matches) > 0 {
			existingVpcPrefix = &matches[0]
		}

		// If the VPC Prefix was found in the DB, we need to check if it is valid
		if existingVpcPrefix != nil {
			if existingVpcPrefix.SiteID != site.ID {
				logger.Warn().Msgf("unable to create VPC Prefix found on Site: VPC Prefix ID already exists under a different Site for VPC Prefix %s", controllerVpcPrefixID)
				return nil, nil
			}
			if existingVpcPrefix.Deleted == nil {
				return existingVpcPrefix, nil
			}
			if existingVpcPrefix.VpcID != vpc.ID {
				logger.Warn().Msgf("unable to create VPC Prefix found on Site: VPC differs in REST cache and Site record for VPC Prefix %s", controllerVpcPrefixID)
				return nil, nil
			}
			if existingVpcPrefix.Org != vpc.Org {
				logger.Warn().Msgf("unable to create VPC Prefix found on Site: tenant organization differs in REST cache and Site record %s", vpc.Org)
				return nil, nil
			}
			// Clear restores the row as stored; do not acquire a Site CIDR that disagrees with
			// the cached Prefix/VpcID or DB and IPAM will permanently diverge.
			existingPrefix, parseErr := netip.ParsePrefix(existingVpcPrefix.Prefix)
			if parseErr != nil || existingPrefix.Masked().String() != reportedVpcPrefix.Prefix {
				logger.Warn().Msgf("unable to create VPC Prefix found on Site: prefix differs in REST cache and Site record for VPC Prefix %s", controllerVpcPrefixID)
				return nil, nil
			}
			// Deleted records when the delete happened, so a delete newer than the interval can
			// postdate this inventory. Undeleting then would revive a VPC Prefix the snapshot
			// never saw removed. Skip before the IPAM work below rather than after, so there is
			// no allocation to unwind. A later inventory undeletes it if the Site still reports it.
			if site.IsTimeWithinStaleInventoryThreshold(*existingVpcPrefix.Deleted) {
				logger.Info().Msgf("not undeleting VPC Prefix %s yet because it was deleted more recently than the inventory interval", controllerVpcPrefixID)
				return nil, nil
			}
		}

		// Get the IP Block for the VPC Prefix
		// if existingVpcPrefix is not nil, we use the stored IP Block ID
		// otherwise we need to find the most specific Ready tenant IPBlock that contains its prefix
		ipBlockDAO := cdbm.NewIPBlockDAO(mvp.dbSession)
		var ipBlock *cdbm.IPBlock
		if existingVpcPrefix != nil {
			// Undelete must preserve the stored association. Re-selecting from all
			// containing blocks could strand the row when a more-specific block appears.
			if existingVpcPrefix.IPBlockID == nil {
				logger.Warn().Msgf("unable to create VPC Prefix found on Site: stored IP Block ID is missing for VPC Prefix %s", existingVpcPrefix.ID)
				return nil, nil
			}

			ipBlock, reloadErr = ipBlockDAO.GetByID(ctx, tx, *existingVpcPrefix.IPBlockID, nil)
			if reloadErr != nil {
				if errors.Is(reloadErr, cdb.ErrDoesNotExist) {
					logger.Warn().Msgf("unable to create VPC Prefix found on Site: stored IP Block %s was not found for VPC Prefix %s", existingVpcPrefix.IPBlockID, existingVpcPrefix.ID)
					return nil, nil
				}
				return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to retrieve stored IP Block, DB error: %w", reloadErr)
			}
			if ipBlock.SiteID != site.ID {
				logger.Warn().Msgf("unable to create VPC Prefix found on Site: stored IP Block belongs to a different Site for VPC Prefix %s", existingVpcPrefix.ID)
				return nil, nil
			}
			if ipBlock.TenantID == nil || *ipBlock.TenantID != vpc.TenantID {
				logger.Warn().Msgf("unable to create VPC Prefix found on Site: stored IP Block belongs to a different Tenant for VPC Prefix %s", existingVpcPrefix.ID)
				return nil, nil
			}
			if !ipBlock.ContainsPrefix(reportedPrefix) {
				logger.Warn().Msgf("unable to create VPC Prefix found on Site: stored IP Block does not contain Prefix %s for VPC Prefix %s", reportedPrefix, existingVpcPrefix.ID)
				return nil, nil
			}
		} else {
			// Site inventory does not report the REST IPBlock ID for a new VPC Prefix.
			// Find the most specific Ready tenant IPBlock that contains its prefix.
			ipBlocks, _, ipBlockErr := ipBlockDAO.GetAll(ctx, tx, cdbm.IPBlockFilterInput{
				SiteIDs:   []uuid.UUID{site.ID},
				TenantIDs: []uuid.UUID{vpc.TenantID},
				Statuses:  []string{cdbm.IPBlockStatusReady},
			}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
			if ipBlockErr != nil {
				return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to retrieve IP Blocks, DB error: %w", ipBlockErr)
			}

			for i := range ipBlocks {
				candidateIPBlock := &ipBlocks[i]
				// A fully granted IPBlock is entirely owned by an existing VPC Prefix,
				// but the IPAM DB reports it as empty and its Status stays Ready.
				if candidateIPBlock.FullGrant || !candidateIPBlock.ContainsPrefix(reportedPrefix) {
					continue
				}
				if ipBlock == nil || candidateIPBlock.PrefixLength > ipBlock.PrefixLength {
					ipBlock = candidateIPBlock
				}
			}
			if ipBlock == nil {
				logger.Warn().Msgf("unable to create VPC Prefix found on Site: no containing IP Block was found for Prefix: %s", reportedPrefix)
				return nil, nil
			}
		}

		// Claim the exact Site-reported CIDR in REST IPAM while holding the same
		// tenant/IPBlock lock used by the normal REST create path.
		lockErr := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(fmt.Sprintf("%s-%s", vpc.TenantID.String(), ipBlock.ID.String())), false)
		if lockErr != nil {
			return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to acquire advisory lock on IP Block, DB error: %w", lockErr)
		}

		// Refresh FullGrant under the lock; the candidate scan above read it before the
		// lock, so a concurrent create may have flipped it since. Both IPAM helpers gate
		// on this flag, and a full grant acquires no child prefix, so the usage check in
		// CreateChildIpamEntryForIPBlock cannot catch a stale value.
		freshIPBlock, reloadIPBlockErr := cdbm.NewIPBlockDAO(mvp.dbSession).GetByID(ctx, tx, ipBlock.ID, nil)
		if reloadIPBlockErr != nil {
			return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to reload IP Block under advisory lock, DB error: %w", reloadIPBlockErr)
		}
		ipBlock = freshIPBlock
		if ipBlock.Status != cdbm.IPBlockStatusReady {
			logger.Warn().Msgf("unable to create VPC Prefix found on Site: IP Block %s is no longer Ready", ipBlock.ID)
			return nil, nil
		}
		if ipBlock.FullGrant {
			logger.Warn().Msgf("unable to create VPC Prefix found on Site: IP Block %s was fully granted concurrently", ipBlock.ID)
			return nil, nil
		}

		ipamStorage := ipam.NewIpamStorage(mvp.dbSession.DB, tx.GetBunTx())
		reportedPrefixLength := reportedPrefix.Bits()

		// Soft-skips after IPAM mutation must return a non-nil error so WithTxResult
		// rolls back (e.g. FullGrant=true from CreateChildIpamEntryForIPBlock).
		var allocateErr error
		if ipBlock.PrefixLength == reportedPrefixLength {
			_, allocateErr = ipam.CreateChildIpamEntryForIPBlock(
				ctx, tx, mvp.dbSession, ipamStorage, ipBlock, reportedPrefixLength,
			)
		} else {
			_, allocateErr = ipam.AcquireSpecificChildIpamEntryForIPBlock(
				ctx, tx, mvp.dbSession, ipamStorage, ipBlock, reportedVpcPrefix.Prefix,
			)
		}
		if allocateErr != nil {
			return nil, fmt.Errorf(
				"unable to create VPC Prefix found on Site: failed to create IPAM entry for VPC Prefix: %w",
				allocateErr,
			)
		}

		// If the VPC Prefix was soft-deleted, undelete it
		// reuse the existing VPC Prefix ID and Status from the Site inventory to avoid creating a new VPC Prefix
		if existingVpcPrefix != nil {
			// Soft-deleted rows almost always carry Status=Deleting (set by the REST
			// delete path before soft-delete). Clear only nulls deleted; also reset
			// Status from Site inventory here. UpdateVpcPrefixesInDB skips refresh when
			// Status is Deleting and Site reports a non-terminal state (e.g. READY).
			restored, clearErr := vpcPrefixDAO.Clear(ctx, tx, cdbm.VpcPrefixClearInput{VpcPrefixID: existingVpcPrefix.ID, Deleted: true})
			if clearErr != nil {
				return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to clear soft-delete timestamp for VPC Prefix, DB error: %w", clearErr)
			}
			status, statusMessage := getControllerVpcPrefixStatus(controllerVpcPrefix.GetStatus())
			updated, updateErr := vpcPrefixDAO.Update(ctx, tx, cdbm.VpcPrefixUpdateInput{
				VpcPrefixID: restored.ID,
				Status:      &status,
			})
			if updateErr != nil {
				return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to update VPC Prefix status after undelete, DB error: %w", updateErr)
			}
			_, statusErr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
				EntityID: updated.ID.String(), Status: status, Message: &statusMessage,
			})
			if statusErr != nil {
				return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to create Status Detail after undelete, DB error: %w", statusErr)
			}
			return updated, nil
		}

		// If an active VPC Prefix already uses this name for the Tenant/Site, append a recovered suffix.
		nameConflictVpcPrefixes, _, nameErr := vpcPrefixDAO.GetAll(ctx, tx, cdbm.VpcPrefixFilterInput{
			Names: []string{reportedVpcPrefix.Name}, TenantIDs: []uuid.UUID{vpc.TenantID}, SiteIDs: []uuid.UUID{site.ID},
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if nameErr != nil {
			return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to retrieve VPC Prefix by name, DB error: %w", nameErr)
		}
		if len(nameConflictVpcPrefixes) > 0 {
			reportedVpcPrefix.Name = fmt.Sprintf("%s-recovered-%s", reportedVpcPrefix.Name, reportedVpcPrefix.ID.String()[:8])
		}

		// If the VPC Prefix was not soft-deleted, create a new VPC Prefix
		// use the controller VPC Prefix ID and Status from the Site inventory to avoid creating a new VPC Prefix
		readyMsg := "VPC Prefix was found on Site, Ready for use"
		created, createErr := vpcPrefixDAO.Create(ctx, tx, cdbm.VpcPrefixCreateInput{
			VpcPrefixID:  &controllerVpcPrefixID,
			Name:         reportedVpcPrefix.Name,
			TenantOrg:    vpc.Org,
			SiteID:       site.ID,
			VpcID:        vpc.ID,
			TenantID:     vpc.TenantID,
			IpBlockID:    &ipBlock.ID,
			Prefix:       reportedVpcPrefix.Prefix,
			PrefixLength: reportedPrefixLength,
			Status:       cdbm.VpcPrefixStatusReady,
			CreatedBy:    vpc.CreatedBy,
		})
		if createErr != nil {
			return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to create VPC Prefix, DB error: %w", createErr)
		}
		_, statusErr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: created.ID.String(), Status: cdbm.VpcPrefixStatusReady, Message: &readyMsg,
		})
		if statusErr != nil {
			return nil, fmt.Errorf("unable to create VPC Prefix found on Site: failed to create Status Detail, DB error: %w", statusErr)
		}
		return created, nil
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to recover VPC Prefix from Site inventory")
		return nil
	}
	return vpcPrefix
}

// getControllerVpcPrefixStatus maps Controller VPC Prefix tenant state into REST status and status-detail text.
func getControllerVpcPrefixStatus(status *corev1.VpcPrefixStatus) (string, string) {
	// Older Controller builds did not report status; inventory presence meant ready.
	if status == nil {
		return cdbm.VpcPrefixStatusReady, "VPC Prefix is ready for use"
	}

	switch status.GetTenantState() {
	case corev1.TenantState_PROVISIONING:
		return cdbm.VpcPrefixStatusProvisioning, "VPC Prefix is being provisioned on Site"
	case corev1.TenantState_READY:
		return cdbm.VpcPrefixStatusReady, "VPC Prefix is ready for use"
	case corev1.TenantState_CONFIGURING:
		return cdbm.VpcPrefixStatusProvisioning, "VPC Prefix is being configured on Site"
	case corev1.TenantState_TERMINATING:
		return cdbm.VpcPrefixStatusDeleting, "VPC Prefix is being deleted on Site"
	case corev1.TenantState_TERMINATED:
		return cdbm.VpcPrefixStatusDeleted, "VPC Prefix has been deleted on Site"
	case corev1.TenantState_FAILED:
		return cdbm.VpcPrefixStatusError, "VPC Prefix is in error state"
	default:
		return cdbm.VpcPrefixStatusError, "VPC Prefix status is unknown"
	}
}

// deleteVpcPrefixFromDB is a helper function to delete VPC Prefix from DB
func (mvp ManageVpcPrefix) deleteVpcPrefixFromDB(ctx context.Context, tx *cdb.Tx, vpcPrefix *cdbm.VpcPrefix, logger zerolog.Logger) error {
	// Acquire an advisory lock on the parent IP block ID on which there could be contention
	// this lock is released when the transaction commits or rollsback
	err := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(vpcPrefix.IPBlockID.String()), false)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to acquire advisory lock on IP Block")
		return err
	}
	logger.Info().Msg("acquired advisory lock on IP Block for VPC Prefix")

	// Delete IPAM entry for this subnet
	ipamStorage := ipam.NewIpamStorage(mvp.dbSession.DB, tx.GetBunTx())
	err = ipam.DeleteChildIpamEntryFromCidr(ctx, tx, mvp.dbSession, ipamStorage, vpcPrefix.IPBlock, vpcPrefix.Prefix)
	if err != nil {
		logger.Error().Err(err).Msg("failed to delete ipam record for Subnet")
		return err
	}
	logger.Info().Msg("deleted VPC Prefix IPAM entry")

	// Soft-delete VPC Prefix
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(mvp.dbSession)

	err = vpcPrefixDAO.Delete(ctx, tx, vpcPrefix.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to delete VPC Prefix from DB")
		return err
	}

	return nil
}

// updateVpcPrefixStatusInDB is helper function to write VpcPrefix updates to DB
func (mvp ManageVpcPrefix) updateVpcPrefixStatusInDB(ctx context.Context, tx *cdb.Tx, vpcPrefixID uuid.UUID, status *string, statusMessage *string) error {
	if status != nil {
		VpcPrefixDAO := cdbm.NewVpcPrefixDAO(mvp.dbSession)

		_, err := VpcPrefixDAO.Update(ctx, tx, cdbm.VpcPrefixUpdateInput{VpcPrefixID: vpcPrefixID, Status: status})
		if err != nil {
			return err
		}

		statusDetailDAO := cdbm.NewStatusDetailDAO(mvp.dbSession)
		_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: vpcPrefixID.String(), Status: *status, Message: statusMessage})
		if err != nil {
			return err
		}
	}
	return nil
}

// NewManageVpcPrefix returns a new ManageVpcPrefix activity
func NewManageVpcPrefix(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageVpcPrefix {
	return ManageVpcPrefix{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
