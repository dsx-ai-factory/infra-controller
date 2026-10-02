// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	"errors"
	"fmt"
	"net/netip"

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

// UpdateSitePrefixesInDB reconciles each reported prefix in its own transaction.
// Publication, absence processing, and retiring the legacy importer belong to
// the subsequent complete-inventory implementation.
func (manager ManageSitePrefix) UpdateSitePrefixesInDB(ctx context.Context, siteID uuid.UUID, inventory *corev1.SitePrefixInventory) error {
	logger := log.With().Str("Activity", "UpdateSitePrefixesInDB").Str("Site ID", siteID.String()).Logger()
	logger.Info().Msg("starting activity")

	if inventory == nil {
		return invalid("UpdateSitePrefixesInDB called with nil inventory")
	}
	if inventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Str("Status Message", inventory.StatusMsg).
			Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}
	if inventory.InventoryStatus != corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS {
		return invalid("unsupported SitePrefix inventory status")
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
	if page != nil {
		logger.Info().Msgf("Received SitePrefix inventory page: %d of %d, page size: %d, total count: %d",
			page.CurrentPage, page.TotalPages, page.PageSize, page.TotalItems)
	}

	for _, prefix := range inventory.SitePrefixes {
		// Validate only the current prefix so a later invalid entry cannot prevent
		// earlier valid entries from being reconciled. Page IDs do not imply absence.
		err := validatePrefix(prefix)
		if err != nil {
			logger.Warn().Err(err).Str("Site Prefix ID", prefix.GetId().GetValue()).
				Msg("received invalid SitePrefix")
			return err
		}
		// Keep one prefix's REST identity, IPAM changes, and status detail atomic.
		// A failed prefix must not roll back earlier prefixes in the page.
		deferred, err := cdb.WithTxResult(ctx, manager.dbSession, func(tx *cdb.Tx) (bool, error) {
			err := tx.AcquireAdvisoryLock(ctx, cdbm.SiteFabricIPBlockLockID(site.InfrastructureProviderID, siteID), false)
			if err != nil {
				return false, err
			}
			lockedSite, err := siteDAO.GetByIDForUpdate(ctx, tx, siteID)
			if err != nil {
				return false, err
			}
			if lockedSite.InfrastructureProviderID != site.InfrastructureProviderID {
				return false, invalid("Site provider changed while acquiring inventory lock")
			}
			return manager.reconcile(ctx, tx, lockedSite, prefix)
		})
		if err != nil {
			logger.Error().Err(err).Str("Site Prefix ID", prefix.GetId().GetValue()).
				Msg("failed to reconcile SitePrefix in DB")
			return err
		}
		if deferred {
			logger.Warn().Str("Site Prefix ID", prefix.GetId().GetValue()).
				Msg("Deferring operator SitePrefix replacement until complete inventory")
		}
	}
	logger.Info().Msg("completing activity")
	return nil
}

// reconcile applies one validated Core prefix to its linked REST IP Block, adopting
// a compatible unlinked operator root or creating a block when necessary. The
// caller holds the Site fabric and active Site row locks in tx; all IP Block,
// IPAM, and status-detail writes participate in that transaction.
// It returns true without writes when an operator replacement must wait for its
// predecessor to be removed. It never removes roots or infers absence.
func (manager ManageSitePrefix) reconcile(ctx context.Context, tx *cdb.Tx, site *cdbm.Site, prefix *corev1.SitePrefix) (bool, error) {
	id := uuid.MustParse(prefix.GetId().GetValue()) // validated before the transaction
	cidr := netip.MustParsePrefix(prefix.Config.Prefix)
	family := cdbm.IPBlockProtocolVersionV6
	if cidr.Addr().Is4() {
		family = cdbm.IPBlockProtocolVersionV4
	}
	dao := cdbm.NewIPBlockDAO(manager.dbSession)
	// Include deleted identities so a retired Core ID cannot be reused.
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
		// Existing links may change metadata and lifecycle, but not ownership or CIDR.
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
		// Adopt only an exact, compatible, unlinked operator root. Linking it in
		// place preserves its REST ID, IPAM tree, and existing allocations.
		roots, _, err := dao.GetAll(ctx, tx, cdbm.IPBlockFilterInput{SiteIDs: []uuid.UUID{site.ID}, ExcludeDerived: true},
			paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
		if err != nil {
			return false, err
		}
		var exact []cdbm.IPBlock
		deferReplacement := false
		for _, root := range roots {
			other, err := netip.ParsePrefix(fmt.Sprintf("%s/%d", root.Prefix, root.PrefixLength))
			if err != nil {
				return false, invalid("existing operator IP Block %s has an invalid prefix", root.ID)
			}
			if other.Masked() == cidr {
				exact = append(exact, root)
			} else if other.Overlaps(cidr) {
				if root.SitePrefixID == nil || root.InfrastructureProviderID != site.InfrastructureProviderID ||
					root.ProtocolVersion != family || root.RoutingType != cdbm.IPBlockRoutingTypeDatacenterOnly {
					return false, invalid("operator SitePrefix %s overlaps an existing root", id)
				}
				// Preserve reported lifecycle changes regardless of which identity arrives first.
				// The replacement must wait until its linked predecessor is removed.
				deferReplacement = true
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
			if candidate.SitePrefixID != nil || deferReplacement {
				return true, nil
			}
			block, err = dao.LinkSitePrefix(ctx, tx, candidate.ID, id)
			if err != nil {
				return false, err
			}
		}
		if deferReplacement {
			return true, nil
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
		// Only operator roots need a cloud-IPAM entry here; tenant prefixes are
		// represented by private IP Blocks without a cloud-IPAM tree.
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
		// Write only changed fields so replaying an unchanged report is a no-op.
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
	// Record the initial status or a change from the stored status.
	if previousStatus != status {
		_, err = cdbm.NewStatusDetailDAO(manager.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: block.ID.String(), Status: status, Message: cutil.GetPtr("Reported by Core SitePrefix inventory"),
		})
	}
	return false, err
}
