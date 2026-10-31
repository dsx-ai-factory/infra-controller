// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	"errors"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/rs/zerolog"
)

const sitePrefixMissingMessage = "SitePrefix missing from Core inventory"

// reconcileAbsence uses the final page's full ID list without waiting for earlier
// pages. Recently created blocks are protected by the Site's inventory threshold.
func (manager ManageSitePrefix) reconcileAbsence(ctx context.Context, site *cdbm.Site, inventory *corev1.SitePrefixInventory, logger zerolog.Logger) error {
	if !util.ShouldReconcileDeletions(inventory.InventoryPage) {
		return nil
	}
	reported := make(map[string]bool)
	for _, id := range inventory.GetInventoryPage().GetItemIds() {
		reported[id] = true
	}
	for _, prefix := range inventory.SitePrefixes {
		reported[prefix.GetId().GetValue()] = true
	}
	dao := cdbm.NewIPBlockDAO(manager.dbSession)
	blocks, _, err := dao.GetAll(ctx, nil, cdbm.IPBlockFilterInput{
		SiteIDs: []uuid.UUID{site.ID}, CoreLinkedOnly: true,
	}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	if err != nil {
		return err
	}
	for _, candidate := range blocks {
		if candidate.SitePrefixID == nil || reported[candidate.SitePrefixID.String()] {
			continue
		}
		if ctx.Err() != nil {
			return ctx.Err()
		}
		isLockContention := false
		err = cdb.WithTx(ctx, manager.dbSession, func(tx *cdb.Tx) error {
			err := tx.TryAcquireAdvisoryLock(ctx, cdbm.SiteFabricIPBlockLockID(site.InfrastructureProviderID, site.ID), nil)
			if err != nil {
				// The retry aggregate includes earlier failures; classify its last attempt.
				lastErr := errors.Unwrap(err)
				if lastErr == nil {
					lastErr = err
				}
				isLockContention = errors.Is(lastErr, cdb.ErrXactAdvisoryLockFailed)
				return err
			}
			lockedSite, err := cdbm.NewSiteDAO(manager.dbSession).GetByID(ctx, tx, site.ID, nil, false, true)
			if err != nil {
				return err
			}
			block, err := dao.GetByIDForUpdate(ctx, tx, candidate.ID)
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return nil
			}
			if err != nil {
				return err
			}
			if block.SitePrefixID == nil || reported[block.SitePrefixID.String()] ||
				block.InfrastructureProviderID != lockedSite.InfrastructureProviderID ||
				lockedSite.IsTimeWithinStaleInventoryThreshold(block.Created) ||
				(block.Managed && block.TenantID != nil) {
				return nil
			}
			if block.Managed && block.TenantID == nil {
				err = manager.updateStatus(ctx, tx, block, cdbm.IPBlockStatusDeleting, sitePrefixMissingMessage)
				if err != nil {
					return err
				}
				return manager.removeOperatorRoot(ctx, tx, block, true, logger)
			}
			if !block.Managed && block.TenantID != nil {
				if block.Status == cdbm.IPBlockStatusDeleting {
					return dao.Delete(ctx, tx, block.ID)
				}
				return manager.updateStatus(ctx, tx, block, cdbm.IPBlockStatusError, sitePrefixMissingMessage)
			}
			return nil
		})
		if err != nil {
			if isLockContention {
				logger.Warn().Err(err).Str("IP Block ID", candidate.ID.String()).
					Msg("skipping missing SitePrefix after bounded Site fabric lock contention retries")
				continue
			}
			logger.Error().Err(err).Str("IP Block ID", candidate.ID.String()).Msg("failed to reconcile missing SitePrefix")
		}
	}
	return ctx.Err()
}

func (manager ManageSitePrefix) updateStatus(ctx context.Context, tx *cdb.Tx, block *cdbm.IPBlock, status, message string) error {
	if block.Status == status {
		return nil
	}
	_, err := cdbm.NewIPBlockDAO(manager.dbSession).Update(ctx, tx, cdbm.IPBlockUpdateInput{IPBlockID: block.ID, Status: &status})
	if err != nil {
		return err
	}
	_, err = cdbm.NewStatusDetailDAO(manager.dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
		EntityID: block.ID.String(), Status: status, Message: &message,
	})
	return err
}

// The caller holds the Site and root locks. IPAM and the REST row retire together.
// A hard-deleted Core identity is unlinked while allocations drain so a new ID
// for the same CIDR can adopt the existing root and its allocations.
func (manager ManageSitePrefix) removeOperatorRoot(ctx context.Context, tx *cdb.Tx, block *cdbm.IPBlock, missing bool, logger zerolog.Logger) error {
	err := tx.WithSavepoint(ctx, func(sp *cdb.Tx) error {
		_, allocationCount, err := cdbm.NewAllocationConstraintDAO(manager.dbSession).GetAll(ctx, sp, cdbm.AllocationConstraintFilterInput{
			ResourceType: cutil.GetPtr(cdbm.AllocationResourceTypeIPBlock), ResourceTypeIDs: []uuid.UUID{block.ID},
		}, paginator.PageInput{}, nil)
		if err != nil {
			return err
		}
		storage := ipam.NewIpamStorage(manager.dbSession.DB, sp.GetBunTx())
		cidr := ipam.GetCidrForIPBlock(ctx, block.Prefix, block.PrefixLength)
		namespace := ipam.GetIpamNamespaceForIPBlock(ctx, block.RoutingType, block.InfrastructureProviderID.String(), block.SiteID.String())
		prefix, err := storage.ReadPrefix(ctx, cidr, namespace)
		if err != nil {
			return err
		}
		dao := cdbm.NewIPBlockDAO(manager.dbSession)
		if allocationCount > 0 || block.FullGrant || prefix.HasAllocations() {
			if missing {
				_, err = dao.Clear(ctx, sp, cdbm.IPBlockClearInput{IPBlockID: block.ID, SitePrefixID: true})
			}
			return err
		}
		err = ipam.DeleteIpamEntryForIPBlock(ctx, storage, block.Prefix, block.PrefixLength,
			block.RoutingType, block.InfrastructureProviderID.String(), block.SiteID.String())
		if err != nil {
			return err
		}
		return dao.Delete(ctx, sp, block.ID)
	})
	if errors.Is(err, cdb.ErrTransactionSavepoint) {
		return err
	}
	if err != nil {
		// Cleanup rolled back, but commit Deleting so allocations remain blocked.
		logger.Error().Err(err).Str("IP Block ID", block.ID.String()).Msg("failed to retire operator root; will retry on inventory")
	}
	return nil
}
