// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"strings"
	"unicode"
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

const (
	sitePrefixInventoryStatusMessage = "Reported by Core SitePrefix inventory"
	invalidSitePrefixInventory       = "InvalidSitePrefixInventory"
	sitePrefixNameMaxLength          = 256
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
// It looks up the active IP Block, creates, adopts, or restores one when missing, then
// applies lifecycle without changing REST metadata or the block's Site, owner, or CIDR.
// Unreconcilable entries are logged and skipped; later entries still proceed.
// Publication, absence processing, and retiring the legacy importer belong to
// the follow-up SitePrefix publication and retirement implementation.
func (manager ManageSitePrefix) UpdateSitePrefixesInDB(ctx context.Context, siteID uuid.UUID, inventory *corev1.SitePrefixInventory) error {
	logger := log.With().Str("Activity", "UpdateSitePrefixesInDB").Str("Site ID", siteID.String()).Logger()
	logger.Info().Msg("starting activity")

	if inventory == nil {
		return invalidSitePrefixTemporalApplicationError("UpdateSitePrefixesInDB called with nil inventory")
	}
	if inventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Str("Status Message", inventory.StatusMsg).
			Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}
	if inventory.InventoryStatus != corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS {
		return invalidSitePrefixTemporalApplicationError("unsupported SitePrefix inventory status")
	}
	siteDAO := cdbm.NewSiteDAO(manager.dbSession)
	site, err := siteDAO.GetByID(ctx, nil, siteID, nil, false, false)
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
	// Load the IP Blocks linked to this page's SitePrefix IDs once, as other inventory receivers do.
	// SitePrefix IDs are global, so validate ownership before changing a matched IP Block.
	prefixIDs := []uuid.UUID{}
	for _, prefix := range inventory.SitePrefixes {
		id, err := uuid.Parse(prefix.GetId().GetValue())
		if err == nil {
			prefixIDs = append(prefixIDs, id)
		}
	}
	dao := cdbm.NewIPBlockDAO(manager.dbSession)
	blocks, _, err := dao.GetAll(ctx, nil, cdbm.IPBlockFilterInput{SitePrefixIDs: prefixIDs},
		paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve IP Blocks for SitePrefix inventory")
		return err
	}
	existingBlockMap := make(map[uuid.UUID]*cdbm.IPBlock, len(blocks))
	for i := range blocks {
		existingBlockMap[*blocks[i].SitePrefixID] = &blocks[i]
	}

	for _, prefix := range inventory.SitePrefixes {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		// Validate per entry so a malformed report cannot hold back the rest of
		// the page. Page IDs do not imply absence.
		err := validatePrefix(prefix)
		if err != nil {
			logger.Warn().Err(err).Str("Site Prefix ID", prefix.GetId().GetValue()).
				Msg("received invalid SitePrefix")
			continue
		}
		// Keep one prefix's IP Block, IPAM changes, and status detail atomic.
		// A failed prefix must not roll back earlier prefixes in the page.
		isLockContention := false
		id := uuid.MustParse(prefix.GetId().GetValue()) // validated before the transaction
		var reconciledBlock *cdbm.IPBlock
		err = cdb.WithTx(ctx, manager.dbSession, func(tx *cdb.Tx) error {
			err := tx.TryAcquireAdvisoryLock(ctx, cdbm.SiteFabricIPBlockLockID(site.InfrastructureProviderID, siteID), nil)
			if err != nil {
				// The retry aggregate includes earlier failures; classify its last attempt.
				lastErr := errors.Unwrap(err)
				if lastErr == nil {
					lastErr = err
				}
				isLockContention = errors.Is(lastErr, cdb.ErrXactAdvisoryLockFailed)
				return err
			}
			lockedSite, err := siteDAO.GetByID(ctx, tx, siteID, nil, false, true)
			if err != nil {
				return err
			}
			if lockedSite.InfrastructureProviderID != site.InfrastructureProviderID {
				return invalidSitePrefixTemporalApplicationError("Site provider changed while acquiring inventory lock")
			}
			block := existingBlockMap[id]
			if block != nil {
				// Reload the page snapshot under the row lock before applying lifecycle.
				block, err = dao.GetByIDForUpdate(ctx, tx, block.ID)
				if err != nil && !errors.Is(err, cdb.ErrDoesNotExist) {
					return err
				}
				if errors.Is(err, cdb.ErrDoesNotExist) {
					block = nil
				}
			}
			var tenantID *uuid.UUID
			if prefix.Status.Authority == corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED {
				tenants, _, err := cdbm.NewTenantDAO(manager.dbSession).GetAll(ctx, tx,
					cdbm.TenantFilterInput{Orgs: []string{prefix.Config.GetTenantOrganizationId()}},
					paginator.PageInput{Limit: cutil.GetPtr(2)}, nil)
				if err != nil {
					return err
				}
				if len(tenants) != 1 {
					return invalidSitePrefixTemporalApplicationError("tenant organization for SitePrefix %s is unknown or ambiguous", id)
				}
				tenantID = &tenants[0].ID
			}
			if block == nil {
				block, err = manager.createOrUpdateSitePrefixFromSite(ctx, tx, lockedSite, prefix, tenantID)
				if err != nil {
					return err
				}
				if block == nil {
					return nil
				}
			} else {
				// Existing links may change lifecycle, but not ownership or CIDR.
				err = validateIPBlockMatchesSitePrefix(block, lockedSite, prefix, tenantID)
				if err != nil {
					return err
				}
			}

			// REST owns metadata after creation, including on adopted and restored
			// roots. Only lifecycle changes produce writes and status details.
			status := getSitePrefixStatus(prefix.Status.LifecycleState)
			if status != block.Status {
				block, err = dao.Update(ctx, tx, cdbm.IPBlockUpdateInput{IPBlockID: block.ID, Status: &status})
				if err != nil {
					return err
				}
				_, err = cdbm.NewStatusDetailDAO(manager.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
					EntityID: block.ID.String(), Status: status, Message: cutil.GetPtr(sitePrefixInventoryStatusMessage),
				})
				if err != nil {
					return err
				}
			}
			reconciledBlock = block
			return nil
		})
		if err != nil {
			if isLockContention {
				logger.Warn().Err(err).Str("Site Prefix ID", prefix.GetId().GetValue()).
					Msg("skipping SitePrefix after bounded Site fabric lock contention retries")
				continue
			}
			var applicationErr *temporal.ApplicationError
			if errors.As(err, &applicationErr) && applicationErr.Type() == invalidSitePrefixInventory {
				logger.Warn().Err(err).Str("Site Prefix ID", prefix.GetId().GetValue()).
					Msg("skipping unreconcilable SitePrefix")
				continue
			}
			logger.Error().Err(err).Str("Site Prefix ID", prefix.GetId().GetValue()).
				Msg("failed to reconcile SitePrefix in DB")
			continue
		}
		if reconciledBlock != nil {
			// Publish the new snapshot only after the per-entry transaction commits.
			existingBlockMap[id] = reconciledBlock
		}
	}
	logger.Info().Msg("completing activity")
	return ctx.Err()
}

// createOrUpdateSitePrefixFromSite creates, adopts, or restores the IP Block for
// a validated SitePrefix whose ID has no active IP Block. It returns a nil block
// without an error when it skips the SitePrefix. The caller must hold the Site
// fabric and Site row locks, and applies lifecycle changes in the same transaction.
//
// Adoption links an existing unlinked root with the same CIDR, keeping its REST
// ID, IPAM entry, and Allocations. Restoration undeletes the same IP Block and
// recreates IPAM only for an operator root. A soft-deleted block stays deleted
// while Core reports it as Deleting, and a Tenant block also stays deleted if this
// inventory may predate its deletion. An operator prefix reported as Deleting is
// not created if there is no root to adopt. This never removes IP Blocks or infers absence.
func (manager ManageSitePrefix) createOrUpdateSitePrefixFromSite(ctx context.Context, tx *cdb.Tx, site *cdbm.Site, prefix *corev1.SitePrefix, tenantID *uuid.UUID) (*cdbm.IPBlock, error) {
	id := uuid.MustParse(prefix.GetId().GetValue()) // validated before the transaction
	cidr := netip.MustParsePrefix(prefix.Config.Prefix)
	routingType := cdbm.GetSiteFabricIPBlockRoutingType(cidr)
	family := cdbm.IPBlockProtocolVersionV6
	if cidr.Addr().Is4() {
		family = cdbm.IPBlockProtocolVersionV4
	}
	dao := cdbm.NewIPBlockDAO(manager.dbSession)
	// Recheck under the Site locks for a concurrent creation or a soft-deleted IP Block
	// with this SitePrefix ID, following the inventory create/undelete pattern.
	matches, _, err := dao.GetAll(ctx, tx, cdbm.IPBlockFilterInput{
		SitePrefixIDs: []uuid.UUID{id}, IncludeDeleted: true,
	}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	if err != nil {
		return nil, err
	}
	var existingBlock *cdbm.IPBlock
	if len(matches) > 0 {
		existingBlock = &matches[0]
	}
	logger := log.With().Str("Activity", "UpdateSitePrefixesInDB").Str("Site ID", site.ID.String()).
		Str("Site Prefix ID", id.String()).Logger()
	if existingBlock != nil {
		err := validateIPBlockMatchesSitePrefix(existingBlock, site, prefix, tenantID)
		if err != nil {
			return nil, err
		}
		if existingBlock.Deleted == nil {
			return dao.GetByIDForUpdate(ctx, tx, existingBlock.ID)
		}
		// REST soft-deleted this block while Core still reports its SitePrefix ID.
		if prefix.Status.LifecycleState == corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING {
			logger.Info().Msg("Skipping retired SitePrefix still reported as Deleting")
			return nil, nil
		}
		// As with SpectrumX inventory, a recent tenant deletion may postdate
		// the snapshot. A later inventory can recover it if Core still reports it.
		if tenantID != nil && site.IsTimeWithinStaleInventoryThreshold(*existingBlock.Deleted) {
			logger.Info().Msg("Not restoring tenant SitePrefix deleted more recently than the inventory interval")
			return nil, nil
		}
	}
	name := prefix.Metadata.Name
	if name == "" {
		name = cidr.String()
	}
	if tenantID == nil {
		// Adopt only an exact, compatible, unlinked operator root. Linking it in
		// place preserves its REST ID, IPAM tree, and existing allocations.
		roots, _, err := dao.GetAll(ctx, tx, cdbm.IPBlockFilterInput{SiteIDs: []uuid.UUID{site.ID}, ExcludeDerived: true},
			paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
		if err != nil {
			return nil, err
		}
		var exact []cdbm.IPBlock
		rootNames := make(map[string]bool, len(roots))
		for _, root := range roots {
			rootNames[root.Name] = true
			other, err := netip.ParsePrefix(fmt.Sprintf("%s/%d", root.Prefix, root.PrefixLength))
			if err != nil {
				return nil, invalidSitePrefixTemporalApplicationError("existing operator IP Block %s has an invalid prefix", root.ID)
			}
			if other.Masked() == cidr {
				exact = append(exact, root)
			} else if other.Overlaps(cidr) {
				return nil, invalidSitePrefixTemporalApplicationError("operator SitePrefix %s overlaps an existing root", id)
			}
		}
		if len(exact) > 1 {
			return nil, invalidSitePrefixTemporalApplicationError("operator SitePrefix %s has ambiguous adoption candidates", id)
		}
		if len(exact) == 1 {
			candidate, err := dao.GetByIDForUpdate(ctx, tx, exact[0].ID)
			if err != nil {
				return nil, err
			}
			err = validateIPBlockMatchesSitePrefix(candidate, site, prefix, tenantID)
			if err != nil {
				return nil, err
			}
			if candidate.SitePrefixID != nil {
				return nil, invalidSitePrefixTemporalApplicationError("operator SitePrefix %s matches root %s, which is already linked to SitePrefix %s", id, candidate.ID, *candidate.SitePrefixID)
			}
			if existingBlock != nil {
				return nil, invalidSitePrefixTemporalApplicationError("operator SitePrefix %s belongs to deleted IP Block %s, but root %s now has its prefix", id, existingBlock.ID, candidate.ID)
			}
			return dao.LinkSitePrefix(ctx, tx, candidate.ID, id)
		}
		if prefix.Status.LifecycleState == corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING {
			logger.Info().Msg("Skipping operator SitePrefix reported as Deleting with no root to adopt")
			return nil, nil
		}
		if existingBlock != nil && rootNames[existingBlock.Name] {
			return nil, invalidSitePrefixTemporalApplicationError("operator SitePrefix %s preserved name %q is already used by another root", id, existingBlock.Name)
		}
		if existingBlock == nil && rootNames[name] {
			// Match inventory recovery naming while respecting the IP Block name limit.
			suffix := fmt.Sprintf("-recovered-%s", id.String()[:8])
			name = name[:min(len(name), sitePrefixNameMaxLength-len(suffix))] + suffix
			if rootNames[name] {
				return nil, invalidSitePrefixTemporalApplicationError("operator SitePrefix %s recovery name %q is already used by another root", id, name)
			}
		}
	}
	var description *string
	if prefix.Metadata.Description != "" {
		description = &prefix.Metadata.Description
	}
	// Only operator roots need a cloud-IPAM entry here; tenant prefixes are
	// represented by private IP Blocks without a cloud-IPAM tree.
	if tenantID == nil {
		storage := ipam.NewIpamStorage(manager.dbSession.DB, tx.GetBunTx())
		_, err := ipam.CreateIpamEntryForIPBlock(ctx, storage, cidr.Addr().String(), cidr.Bits(),
			routingType, site.InfrastructureProviderID.String(), site.ID.String())
		if err != nil {
			return nil, err
		}
	}
	if existingBlock != nil {
		// Restore the soft-deleted IP Block after recreating IPAM for an operator
		// root, or without IPAM for a tenant block. The caller applies lifecycle.
		return dao.Clear(ctx, tx, cdbm.IPBlockClearInput{IPBlockID: existingBlock.ID, Deleted: true})
	}
	status := getSitePrefixStatus(prefix.Status.LifecycleState)
	block, err := dao.Create(ctx, tx, cdbm.IPBlockCreateInput{
		Name: name, Description: description,
		SiteID: site.ID, InfrastructureProviderID: site.InfrastructureProviderID, TenantID: tenantID,
		SitePrefixID: &id, Prefix: cidr.Addr().String(), PrefixLength: cidr.Bits(),
		Managed:         cutil.GetPtr(tenantID == nil),
		ProtocolVersion: family, RoutingType: routingType,
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

func validateIPBlockMatchesSitePrefix(block *cdbm.IPBlock, site *cdbm.Site, prefix *corev1.SitePrefix, tenantID *uuid.UUID) error {
	cidr := netip.MustParsePrefix(prefix.Config.Prefix)
	family := cdbm.IPBlockProtocolVersionV6
	if cidr.Addr().Is4() {
		family = cdbm.IPBlockProtocolVersionV4
	}
	expected := cdbm.IPBlock{
		SiteID: site.ID, InfrastructureProviderID: site.InfrastructureProviderID,
		TenantID: tenantID, Managed: tenantID == nil,
		Prefix: cidr.Addr().String(), PrefixLength: cidr.Bits(),
		ProtocolVersion: family, RoutingType: cdbm.GetSiteFabricIPBlockRoutingType(cidr),
	}
	if block.Key() != expected.Key() {
		return invalidSitePrefixTemporalApplicationError("IP Block %s does not match Core SitePrefix %s", block.ID, prefix.GetId().GetValue())
	}
	return nil
}

func getSitePrefixStatus(state corev1.SitePrefixLifecycleState) string {
	return map[corev1.SitePrefixLifecycleState]string{
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_PROVISIONING: cdbm.IPBlockStatusProvisioning,
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY:        cdbm.IPBlockStatusReady,
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING:     cdbm.IPBlockStatusDeleting,
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_ERROR:        cdbm.IPBlockStatusError,
	}[state]
}

func invalidSitePrefixTemporalApplicationError(format string, args ...any) error {
	return temporal.NewNonRetryableApplicationError(fmt.Sprintf(format, args...), invalidSitePrefixInventory, nil)
}

func validatePrefix(prefix *corev1.SitePrefix) error {
	if prefix == nil || prefix.Config == nil || prefix.Status == nil || prefix.Metadata == nil {
		return invalidSitePrefixTemporalApplicationError("SitePrefix requires config, status, and metadata")
	}
	id, err := uuid.Parse(prefix.GetId().GetValue())
	if err != nil || id == uuid.Nil || id.String() != prefix.GetId().GetValue() {
		return invalidSitePrefixTemporalApplicationError("SitePrefix requires a canonical, nonzero UUID")
	}
	config, status, metadata := prefix.Config, prefix.Status, prefix.Metadata
	cidr, err := netip.ParsePrefix(config.Prefix)
	if err != nil || cidr != cidr.Masked() || cidr.Addr().Is4In6() {
		return invalidSitePrefixTemporalApplicationError("SitePrefix %s requires a network-aligned CIDR", prefix.GetId().GetValue())
	}
	if config.RoutingScope != corev1.SitePrefixRoutingScope_SITE_PREFIX_ROUTING_SCOPE_DATACENTER_ONLY {
		return invalidSitePrefixTemporalApplicationError("unsupported SitePrefix routing scope")
	}
	minName := 0
	switch status.Authority {
	case corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED:
		if config.TenantOrganizationId != nil || (status.LifecycleState != corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY &&
			status.LifecycleState != corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING) {
			return invalidSitePrefixTemporalApplicationError("operator SitePrefix has an owner or unsupported lifecycle")
		}
	case corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED:
		minName = 2
		if config.GetTenantOrganizationId() == "" || !cidr.Addr().Is4() || cidr.Bits() < 8 || cidr.Bits() > 31 {
			return invalidSitePrefixTemporalApplicationError("tenant SitePrefix requires an owner and RFC1918 IPv4 /8 through /31")
		}
		if cdbm.GetSiteFabricIPBlockRoutingType(cidr) != cdbm.IPBlockRoutingTypeDatacenterOnly ||
			status.LifecycleState < corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_PROVISIONING ||
			status.LifecycleState > corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_ERROR {
			return invalidSitePrefixTemporalApplicationError("unsupported tenant SitePrefix range or lifecycle")
		}
	default:
		return invalidSitePrefixTemporalApplicationError("unsupported SitePrefix authority")
	}
	if len(metadata.Name) < minName || len(metadata.Name) > sitePrefixNameMaxLength ||
		strings.ContainsFunc(metadata.Name, func(r rune) bool { return r > unicode.MaxASCII }) ||
		strings.ContainsFunc(metadata.Name, unicode.IsControl) || len(metadata.Description) > 1024 ||
		!utf8.ValidString(metadata.Description) || strings.ContainsRune(metadata.Description, '\x00') || len(metadata.Labels) > 16 {
		return invalidSitePrefixTemporalApplicationError("invalid SitePrefix metadata")
	}
	keys := make(map[string]bool)
	for _, label := range metadata.Labels {
		key := label.GetKey()
		if key == "" || len(key) > 255 || strings.ContainsFunc(key, func(r rune) bool { return r > unicode.MaxASCII }) ||
			len(label.GetValue()) > 255 || keys[key] {
			return invalidSitePrefixTemporalApplicationError("invalid SitePrefix metadata label")
		}
		keys[key] = true
	}
	return nil
}
