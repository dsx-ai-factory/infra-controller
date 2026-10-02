// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"unicode/utf8"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
)

const sitePrefixInventoryStatusMessage = "Reported by Core SitePrefix inventory"

// ManageSitePrefix reconciles reported Core roots without inferring absence.
type ManageSitePrefix struct {
	dbSession *cdb.Session
}

// NewManageSitePrefix creates the Cloud inventory receiver.
func NewManageSitePrefix(session *cdb.Session) ManageSitePrefix {
	return ManageSitePrefix{dbSession: session}
}

// UpdateSitePrefixesInDB reconciles each reported prefix in its own transaction.
// It looks up the linked IP Block, creates or adopts one only when missing, then
// applies reported metadata and lifecycle without changing the block's identity.
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
			id := uuid.MustParse(prefix.GetId().GetValue()) // validated before the transaction
			cidr := netip.MustParsePrefix(prefix.Config.Prefix)
			family := cdbm.IPBlockProtocolVersionV6
			if cidr.Addr().Is4() {
				family = cdbm.IPBlockProtocolVersionV4
			}
			dao := cdbm.NewIPBlockDAO(manager.dbSession)
			// Look up the identity under the Site locks, including deleted links:
			// unlike inventories that undelete resources, a retired Core ID cannot be reused.
			block, err := dao.GetBySitePrefixID(ctx, tx, id)
			if err != nil && !errors.Is(err, cdb.ErrDoesNotExist) {
				return false, err
			}
			if errors.Is(err, cdb.ErrDoesNotExist) {
				block = nil
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
			if block == nil {
				block, err = manager.createOrUpdateSitePrefixFromSite(ctx, tx, lockedSite, prefix, tenantID)
				if err != nil {
					return false, err
				}
				if block == nil {
					return true, nil
				}
			} else {
				// Existing links may change metadata and lifecycle, but not ownership or CIDR.
				if block.Deleted == nil {
					block, err = dao.GetByIDForUpdate(ctx, tx, block.ID)
					if err != nil {
						return false, err
					}
				}
				if block.Deleted != nil || block.SiteID != lockedSite.ID || block.InfrastructureProviderID != lockedSite.InfrastructureProviderID ||
					(block.TenantID == nil) != (tenantID == nil) || (tenantID != nil && *block.TenantID != *tenantID) ||
					block.Prefix != cidr.Addr().String() || block.PrefixLength != cidr.Bits() ||
					block.ProtocolVersion != family || block.RoutingType != cdbm.IPBlockRoutingTypeDatacenterOnly {
					return false, invalid("Core SitePrefix %s conflicts with its immutable REST identity", id)
				}
			}

			// Apply reported metadata and lifecycle to existing, adopted, and newly
			// created blocks. Unchanged reports must not produce writes or status details.
			status := getSitePrefixStatus(prefix.Status.LifecycleState)
			update := cdbm.IPBlockUpdateInput{IPBlockID: block.ID}
			if prefix.Metadata.Name != "" && prefix.Metadata.Name != block.Name {
				update.Name = &prefix.Metadata.Name
			}
			// Empty operator metadata must not erase provider-maintained values.
			// Tenant descriptions remain authoritative, including an explicit clear.
			if (tenantID != nil || prefix.Metadata.Description != "") &&
				(block.Description == nil || prefix.Metadata.Description != *block.Description) {
				update.Description = &prefix.Metadata.Description
			}
			if status != block.Status {
				update.Status = &status
			}
			if update.Name != nil || update.Description != nil || update.Status != nil {
				_, err = dao.Update(ctx, tx, update)
				if err != nil {
					return false, err
				}
			}
			if status != block.Status {
				_, err = cdbm.NewStatusDetailDAO(manager.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
					EntityID: block.ID.String(), Status: status, Message: cutil.GetPtr(sitePrefixInventoryStatusMessage),
				})
				if err != nil {
					return false, err
				}
			}
			return false, nil
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

// createOrUpdateSitePrefixFromSite adopts a compatible unlinked operator root or
// creates an IP Block for a validated prefix with no linked REST identity. The
// caller checks for existing (including deleted) links under the Site fabric and
// active Site row locks, and keeps this helper and subsequent updates in one tx.
// Adoption preserves the REST ID, IPAM tree, and allocations. Only newly created
// blocks receive an initial status detail here; ordinary updates belong to the caller.
// A nil block without an error defers an operator replacement until its predecessor
// is removed. This helper never removes roots or infers absence.
func (manager ManageSitePrefix) createOrUpdateSitePrefixFromSite(ctx context.Context, tx *cdb.Tx, site *cdbm.Site, prefix *corev1.SitePrefix, tenantID *uuid.UUID) (*cdbm.IPBlock, error) {
	id := uuid.MustParse(prefix.GetId().GetValue()) // validated before the transaction
	cidr := netip.MustParsePrefix(prefix.Config.Prefix)
	family := cdbm.IPBlockProtocolVersionV6
	if cidr.Addr().Is4() {
		family = cdbm.IPBlockProtocolVersionV4
	}
	dao := cdbm.NewIPBlockDAO(manager.dbSession)
	if tenantID == nil {
		// Adopt only an exact, compatible, unlinked operator root. Linking it in
		// place preserves its REST ID, IPAM tree, and existing allocations.
		roots, _, err := dao.GetAll(ctx, tx, cdbm.IPBlockFilterInput{SiteIDs: []uuid.UUID{site.ID}, ExcludeDerived: true},
			paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
		if err != nil {
			return nil, err
		}
		var exact []cdbm.IPBlock
		deferReplacement := false
		for _, root := range roots {
			other, err := netip.ParsePrefix(fmt.Sprintf("%s/%d", root.Prefix, root.PrefixLength))
			if err != nil {
				return nil, invalid("existing operator IP Block %s has an invalid prefix", root.ID)
			}
			if other.Masked() == cidr {
				exact = append(exact, root)
			} else if other.Overlaps(cidr) {
				if root.SitePrefixID == nil || root.InfrastructureProviderID != site.InfrastructureProviderID ||
					root.ProtocolVersion != family || root.RoutingType != cdbm.IPBlockRoutingTypeDatacenterOnly {
					return nil, invalid("operator SitePrefix %s overlaps an existing root", id)
				}
				// Preserve reported lifecycle changes regardless of which identity arrives first.
				// The replacement must wait until its linked predecessor is removed.
				deferReplacement = true
			}
		}
		if len(exact) > 1 {
			return nil, invalid("operator SitePrefix %s has ambiguous adoption candidates", id)
		}
		if len(exact) == 1 {
			candidate, err := dao.GetByIDForUpdate(ctx, tx, exact[0].ID)
			if err != nil {
				return nil, err
			}
			if candidate.InfrastructureProviderID != site.InfrastructureProviderID || candidate.ProtocolVersion != family ||
				candidate.RoutingType != cdbm.IPBlockRoutingTypeDatacenterOnly || candidate.SiteID != site.ID ||
				candidate.TenantID != nil || candidate.Prefix != cidr.Addr().String() || candidate.PrefixLength != cidr.Bits() {
				return nil, invalid("operator SitePrefix %s has an incompatible adoption candidate", id)
			}
			if candidate.SitePrefixID != nil || deferReplacement {
				return nil, nil
			}
			return dao.LinkSitePrefix(ctx, tx, candidate.ID, id)
		}
		if deferReplacement {
			return nil, nil
		}
	}
	// Empty operator metadata must not erase provider-maintained values.
	// Tenant descriptions remain authoritative, including an explicit clear.
	var description *string
	if tenantID != nil || prefix.Metadata.Description != "" {
		description = &prefix.Metadata.Description
	}
	// Only operator roots need a cloud-IPAM entry here; tenant prefixes are
	// represented by private IP Blocks without a cloud-IPAM tree.
	if tenantID == nil {
		storage := ipam.NewIpamStorage(manager.dbSession.DB, tx.GetBunTx())
		_, err := ipam.CreateIpamEntryForIPBlock(ctx, storage, cidr.Addr().String(), cidr.Bits(),
			cdbm.IPBlockRoutingTypeDatacenterOnly, site.InfrastructureProviderID.String(), site.ID.String())
		if err != nil {
			return nil, err
		}
	}
	name := prefix.Metadata.Name
	if name == "" {
		name = cidr.String()
	}
	status := getSitePrefixStatus(prefix.Status.LifecycleState)
	block, err := dao.Create(ctx, tx, cdbm.IPBlockCreateInput{
		Name: name, Description: description,
		SiteID: site.ID, InfrastructureProviderID: site.InfrastructureProviderID, TenantID: tenantID,
		SitePrefixID: &id, Prefix: cidr.Addr().String(), PrefixLength: cidr.Bits(),
		ProtocolVersion: family, RoutingType: cdbm.IPBlockRoutingTypeDatacenterOnly,
		Status: status, CreatedBy: &site.CreatedBy,
	})
	if err != nil {
		return nil, err
	}
	_, err = cdbm.NewStatusDetailDAO(manager.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
		EntityID: block.ID.String(), Status: status, Message: cutil.GetPtr(sitePrefixInventoryStatusMessage),
	})
	if err != nil {
		return nil, err
	}
	return block, nil
}

func getSitePrefixStatus(state corev1.SitePrefixLifecycleState) string {
	return map[corev1.SitePrefixLifecycleState]string{
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_PROVISIONING: cdbm.IPBlockStatusProvisioning,
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY:        cdbm.IPBlockStatusReady,
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING:     cdbm.IPBlockStatusDeleting,
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_ERROR:        cdbm.IPBlockStatusError,
	}[state]
}

func invalid(format string, args ...any) error {
	return temporal.NewNonRetryableApplicationError(fmt.Sprintf(format, args...), "InvalidSitePrefixInventory", nil)
}

func validatePrefix(prefix *corev1.SitePrefix) error {
	if prefix == nil || prefix.Config == nil || prefix.Status == nil || prefix.Metadata == nil {
		return invalid("SitePrefix requires config, status, and metadata")
	}
	id, err := uuid.Parse(prefix.GetId().GetValue())
	if err != nil || id == uuid.Nil || id.String() != prefix.GetId().GetValue() {
		return invalid("SitePrefix requires a canonical, nonzero UUID")
	}
	config, status, metadata := prefix.Config, prefix.Status, prefix.Metadata
	cidr, err := netip.ParsePrefix(config.Prefix)
	if err != nil || cidr != cidr.Masked() || cidr.Addr().Is4In6() {
		return invalid("SitePrefix %s requires a network-aligned CIDR", prefix.GetId().GetValue())
	}
	if config.RoutingScope != corev1.SitePrefixRoutingScope_SITE_PREFIX_ROUTING_SCOPE_DATACENTER_ONLY {
		return invalid("unsupported SitePrefix routing scope")
	}
	minName := 0
	switch status.Authority {
	case corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED:
		if config.TenantOrganizationId != nil || (status.LifecycleState != corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY &&
			status.LifecycleState != corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING) {
			return invalid("operator SitePrefix has an owner or unsupported lifecycle")
		}
	case corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED:
		minName = 2
		if config.GetTenantOrganizationId() == "" || !cidr.Addr().Is4() || cidr.Bits() < 8 || cidr.Bits() > 31 {
			return invalid("tenant SitePrefix requires an owner and RFC1918 IPv4 /8 through /31")
		}
		private := false
		for _, root := range []string{"10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"} {
			parent := netip.MustParsePrefix(root)
			private = private || (parent.Bits() <= cidr.Bits() && parent.Contains(cidr.Addr()))
		}
		if !private || status.LifecycleState < corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_PROVISIONING ||
			status.LifecycleState > corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_ERROR {
			return invalid("unsupported tenant SitePrefix range or lifecycle")
		}
	default:
		return invalid("unsupported SitePrefix authority")
	}
	if len(metadata.Name) < minName || len(metadata.Name) > 256 || !ascii(metadata.Name) ||
		len(metadata.Description) > 1024 || !utf8.ValidString(metadata.Description) || len(metadata.Labels) > 16 {
		return invalid("invalid SitePrefix metadata")
	}
	keys := make(map[string]bool)
	for _, label := range metadata.Labels {
		key := label.GetKey()
		if key == "" || len(key) > 255 || !ascii(key) || len(label.GetValue()) > 255 || keys[key] {
			return invalid("invalid SitePrefix metadata label")
		}
		keys[key] = true
	}
	return nil
}

func ascii(value string) bool {
	for _, char := range value {
		if char > 127 {
			return false
		}
	}
	return true
}
