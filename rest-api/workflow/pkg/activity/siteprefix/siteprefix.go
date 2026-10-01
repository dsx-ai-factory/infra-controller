// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"slices"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
)

// ManageSitePrefix reconciles reported Core roots without inferring absence.
type ManageSitePrefix struct {
	dbSession *cdb.Session
}

// NewManageSitePrefix creates the Cloud inventory receiver.
func NewManageSitePrefix(session *cdb.Session) ManageSitePrefix {
	return ManageSitePrefix{dbSession: session}
}

// UpdateSitePrefixesInDB commits one validated page and its receipt atomically.
// A deferred operator replacement records receipt but never steals an existing
// identity. Publication, absence processing, and setting the cutover marker
// belong to the subsequent complete-inventory implementation.
func (manager ManageSitePrefix) UpdateSitePrefixesInDB(ctx context.Context, siteID uuid.UUID, inventory *corev1.SitePrefixInventory) error {
	logger := log.With().Str("Activity", "UpdateSitePrefixesInDB").Str("Site ID", siteID.String()).Logger()
	logger.Info().Msg("starting activity")

	hash, err := validatePage(inventory)
	if err != nil {
		logger.Warn().Err(err).Msg("received invalid SitePrefix inventory")
		return err
	}
	if inventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Str("Status Message", inventory.StatusMsg).
			Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}
	siteDAO := cdbm.NewSiteDAO(manager.dbSession)
	site, err := siteDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Warn().Err(err).Msg("received SitePrefix inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}
	page := inventory.InventoryPage
	logger.Info().Msgf("Received SitePrefix inventory page: %d of %d, page size: %d, total count: %d",
		page.CurrentPage, page.TotalPages, page.PageSize, page.TotalItems)

	err = cdb.WithTx(ctx, manager.dbSession, func(tx *cdb.Tx) error {
		// A lock collision returns to Temporal's bounded retry rather than
		// holding a worker or transaction indefinitely.
		err := tx.AcquireAdvisoryLock(ctx, cdbm.SiteFabricIPBlockLockID(site.InfrastructureProviderID, siteID), false)
		if err != nil {
			return err
		}
		lockedSite, err := siteDAO.GetByIDForUpdate(ctx, tx, siteID)
		if err != nil {
			return err
		}
		if lockedSite.InfrastructureProviderID != site.InfrastructureProviderID {
			return invalid("Site provider changed while acquiring inventory lock")
		}
		reportedAt := inventory.Timestamp.AsTime()
		// Pages share the collection's start time and can arrive minutes apart.
		// Only a newer accepted collection supersedes them, not elapsed age.
		progress := lockedSite.SitePrefixInventoryProgress
		if progress != nil && reportedAt.Before(progress.ReportedAt) {
			logger.Info().Msg("skipping inventory superseded by a newer collection")
			return nil
		}
		if progress == nil || reportedAt.After(progress.ReportedAt) {
			progress = &cdbm.SitePrefixInventoryProgress{
				ReportedAt: reportedAt,
				ItemIDs:    slices.Clone(inventory.InventoryPage.ItemIds),
				Pages:      make(map[int32]cdbm.SitePrefixInventoryPage),
			}
		}
		if !slices.Equal(progress.ItemIDs, page.ItemIds) {
			return invalid("SitePrefix collection changed its declared IDs")
		}
		if receipt, exists := progress.Pages[page.CurrentPage]; exists {
			if receipt.Hash != hash {
				return invalid("SitePrefix page retry changed its contents")
			}
			logger.Info().Msg("skipping previously reconciled inventory page")
			return nil
		}
		ids := make([]string, 0, len(inventory.SitePrefixes))
		for _, prefix := range inventory.SitePrefixes {
			ids = append(ids, prefix.GetId().GetValue())
		}
		final := page.TotalPages == 0 || page.CurrentPage == page.TotalPages
		if progress.FinalPage != 0 && (page.CurrentPage > progress.FinalPage ||
			page.TotalPages > progress.FinalPage || (final && page.CurrentPage != progress.FinalPage)) {
			return invalid("SitePrefix page conflicts with the final page")
		}
		for number, receipt := range progress.Pages {
			if final && number > page.CurrentPage {
				return invalid("SitePrefix final page precedes an accepted page")
			}
			for _, id := range ids {
				if slices.Contains(receipt.ItemIDs, id) {
					return invalid("SitePrefix %s appears in more than one page", id)
				}
			}
			if len(receipt.ItemIDs) > 0 && len(ids) > 0 &&
				((number < page.CurrentPage && receipt.ItemIDs[len(receipt.ItemIDs)-1] >= ids[0]) ||
					(number > page.CurrentPage && ids[len(ids)-1] >= receipt.ItemIDs[0])) {
				return invalid("SitePrefix pages are not ordered by ID")
			}
		}
		finalPage := progress.FinalPage
		if final {
			finalPage = page.CurrentPage
		}
		if finalPage > 0 && len(progress.Pages)+1 == int(finalPage) {
			var received []string
			for number := int32(1); number <= finalPage; number++ {
				if number == page.CurrentPage {
					received = append(received, ids...)
				} else {
					received = append(received, progress.Pages[number].ItemIDs...)
				}
			}
			if !slices.Equal(received, page.ItemIds) {
				return invalid("complete SitePrefix collection does not match declared IDs")
			}
		}
		receipt := cdbm.SitePrefixInventoryPage{Hash: hash, ItemIDs: ids, DeferredIDs: []string{}}
		for _, prefix := range inventory.SitePrefixes {
			deferred, err := manager.reconcile(ctx, tx, lockedSite, prefix)
			if err != nil {
				logger.Error().Err(err).Str("Site Prefix ID", prefix.GetId().GetValue()).
					Msg("failed to reconcile SitePrefix in DB")
				return err
			}
			if deferred {
				id := prefix.GetId().GetValue()
				receipt.DeferredIDs = append(receipt.DeferredIDs, id)
				logger.Warn().Str("Site Prefix ID", id).
					Msg("Deferring operator SitePrefix replacement until complete inventory")
			}
		}
		if progress.Pages == nil {
			progress.Pages = make(map[int32]cdbm.SitePrefixInventoryPage)
		}
		progress.Pages[page.CurrentPage] = receipt
		if final {
			progress.FinalPage = page.CurrentPage
		}
		_, err = siteDAO.Update(ctx, tx, cdbm.SiteUpdateInput{SiteID: siteID, SitePrefixInventoryProgress: progress})
		return err
	})
	if err != nil {
		logger.Error().Err(err).Msg("failed to reconcile SitePrefix inventory page")
		return err
	}
	logger.Info().Msg("completing activity")
	return nil
}

func (manager ManageSitePrefix) reconcile(ctx context.Context, tx *cdb.Tx, site *cdbm.Site, prefix *corev1.SitePrefix) (bool, error) {
	id := uuid.MustParse(prefix.GetId().GetValue()) // validated before the transaction
	cidr := netip.MustParsePrefix(prefix.Config.Prefix)
	family := cdbm.IPBlockProtocolVersionV6
	if cidr.Addr().Is4() {
		family = cdbm.IPBlockProtocolVersionV4
	}
	dao := cdbm.NewIPBlockDAO(manager.dbSession)
	block, err := dao.GetBySitePrefixID(ctx, tx, id)
	if err != nil && !errors.Is(err, cdb.ErrDoesNotExist) {
		return false, err
	}
	if errors.Is(err, cdb.ErrDoesNotExist) {
		block = nil
		err = nil
	}
	var tenantID *uuid.UUID
	if prefix.Status.Authority == corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED {
		tenants, _, err := cdbm.NewTenantDAO(manager.dbSession).GetAll(ctx, tx,
			cdbm.TenantFilterInput{Orgs: []string{prefix.Config.GetTenantOrganizationId()}},
			paginator.PageInput{Limit: cutil.GetPtr(2)}, nil)
		if err != nil {
			return false, err
		}
		if len(tenants) != 1 {
			return false, invalid("tenant organization for SitePrefix %s is unknown or ambiguous", id)
		}
		tenantID = &tenants[0].ID
	}
	status := map[corev1.SitePrefixLifecycleState]string{
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_PROVISIONING: cdbm.IPBlockStatusProvisioning,
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY:        cdbm.IPBlockStatusReady,
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING:     cdbm.IPBlockStatusDeleting,
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_ERROR:        cdbm.IPBlockStatusError,
	}[prefix.Status.LifecycleState]
	if block != nil {
		if block.Deleted == nil {
			block, err = dao.GetByIDForUpdate(ctx, tx, block.ID)
			if err != nil {
				return false, err
			}
		}
		if block.Deleted != nil || block.SiteID != site.ID || block.InfrastructureProviderID != site.InfrastructureProviderID ||
			(block.TenantID == nil) != (tenantID == nil) || (tenantID != nil && *block.TenantID != *tenantID) ||
			block.Prefix != cidr.Addr().String() || block.PrefixLength != cidr.Bits() ||
			block.ProtocolVersion != family || block.RoutingType != cdbm.IPBlockRoutingTypeDatacenterOnly {
			return false, invalid("Core SitePrefix %s conflicts with its immutable REST identity", id)
		}
	} else if tenantID == nil {
		roots, _, err := dao.GetAll(ctx, tx, cdbm.IPBlockFilterInput{SiteIDs: []uuid.UUID{site.ID}, ExcludeDerived: true},
			paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
		if err != nil {
			return false, err
		}
		var exact []cdbm.IPBlock
		for _, root := range roots {
			other, err := netip.ParsePrefix(fmt.Sprintf("%s/%d", root.Prefix, root.PrefixLength))
			if err != nil {
				return false, invalid("existing operator IP Block %s has an invalid prefix", root.ID)
			}
			if other.Masked() == cidr {
				exact = append(exact, root)
			} else if other.Overlaps(cidr) {
				return false, invalid("operator SitePrefix %s overlaps an existing root", id)
			}
		}
		if len(exact) > 1 {
			return false, invalid("operator SitePrefix %s has ambiguous adoption candidates", id)
		}
		if len(exact) == 1 {
			candidate, err := dao.GetByIDForUpdate(ctx, tx, exact[0].ID)
			if err != nil {
				return false, err
			}
			if candidate.InfrastructureProviderID != site.InfrastructureProviderID || candidate.ProtocolVersion != family ||
				candidate.RoutingType != cdbm.IPBlockRoutingTypeDatacenterOnly || candidate.SiteID != site.ID ||
				candidate.TenantID != nil || candidate.Prefix != cidr.Addr().String() || candidate.PrefixLength != cidr.Bits() {
				return false, invalid("operator SitePrefix %s has an incompatible adoption candidate", id)
			}
			if candidate.SitePrefixID != nil {
				return true, nil
			}
			block, err = dao.LinkSitePrefix(ctx, tx, candidate.ID, id)
			if err != nil {
				return false, err
			}
		}
	}
	// Empty operator metadata must not erase provider-maintained values.
	// Tenant descriptions remain authoritative, including an explicit clear.
	var description *string
	if tenantID != nil || prefix.Metadata.Description != "" {
		description = &prefix.Metadata.Description
	}
	previousStatus := ""
	if block == nil {
		if tenantID == nil {
			storage := ipam.NewIpamStorage(manager.dbSession.DB, tx.GetBunTx())
			_, err = ipam.CreateIpamEntryForIPBlock(ctx, storage, cidr.Addr().String(), cidr.Bits(),
				cdbm.IPBlockRoutingTypeDatacenterOnly, site.InfrastructureProviderID.String(), site.ID.String())
			if err != nil {
				return false, err
			}
		}
		name := prefix.Metadata.Name
		if name == "" {
			name = cidr.String()
		}
		block, err = dao.Create(ctx, tx, cdbm.IPBlockCreateInput{
			Name: name, Description: description,
			SiteID: site.ID, InfrastructureProviderID: site.InfrastructureProviderID, TenantID: tenantID,
			SitePrefixID: &id, Prefix: cidr.Addr().String(), PrefixLength: cidr.Bits(),
			ProtocolVersion: family, RoutingType: cdbm.IPBlockRoutingTypeDatacenterOnly,
			Status: status, CreatedBy: &site.CreatedBy,
		})
	} else {
		previousStatus = block.Status
		update := cdbm.IPBlockUpdateInput{IPBlockID: block.ID}
		if prefix.Metadata.Name != "" && prefix.Metadata.Name != block.Name {
			update.Name = &prefix.Metadata.Name
		}
		if description != nil && (block.Description == nil || *description != *block.Description) {
			update.Description = description
		}
		if status != block.Status {
			update.Status = &status
		}
		if update.Name != nil || update.Description != nil || update.Status != nil {
			_, err = dao.Update(ctx, tx, update)
		}
	}
	if err != nil {
		return false, err
	}
	if previousStatus != status {
		_, err = cdbm.NewStatusDetailDAO(manager.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: block.ID.String(), Status: status, Message: cutil.GetPtr("Reported by Core SitePrefix inventory"),
		})
	}
	return false, err
}
