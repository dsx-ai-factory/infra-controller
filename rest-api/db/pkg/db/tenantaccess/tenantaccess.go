// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package tenantaccess resolves a Tenant's access to Sites from Allocations,
// TenantAccounts, and TenantSite overrides. It is shared by the API handlers
// and the workflow activities.
package tenantaccess

import (
	"context"
	"errors"

	"github.com/google/uuid"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// PrivilegeScope narrows where a Tenant's TargetedInstanceCreation
// capability must be effective for a privilege check. For
// HasTargetedInstanceCreation a non-nil scope is required and the fields
// select how the capability is resolved:
//
//   - SiteID set: resolve the effective capability for that exact Site.
//   - InfrastructureProviderID set: privileged if the Ready TenantAccount for
//     that Provider has TargetedInstanceCreation enabled globally.
//
// SiteID and InfrastructureProviderID must not be set together.
type PrivilegeScope struct {
	InfrastructureProviderID *uuid.UUID
	SiteID                   *uuid.UUID
}

// HasTargetedInstanceCreation reports whether the Tenant has the
// TargetedInstanceCreation capability enabled within the given scope. It is
// nil-safe so callers don't have to repeat the tenant nil check, and it
// deliberately does not read the deprecated tenant-level flag
// (tenant.Config.TargetedInstanceCreation), which is superseded by
// TenantAccount.config with per-site TenantSite.config overrides. See
// PrivilegeScope for how scope selects the resolution strategy.
func HasTargetedInstanceCreation(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, tenant *cdbm.Tenant, scope *PrivilegeScope) (bool, error) {
	if tenant == nil {
		return false, nil
	}

	if scope == nil {
		return false, errors.New("scope must be specified when evaluating Tenant's targeted Instance creation capability")
	}

	if scope.SiteID != nil && scope.InfrastructureProviderID != nil {
		return false, errors.New("site ID and infrastructure provider ID cannot be specified together when evaluating Tenant's targeted Instance creation capability")
	}

	siteID := scope.SiteID
	providerID := scope.InfrastructureProviderID
	var siteOverride *bool

	// Site-scoped: resolve the effective capability for the exact Site.
	if siteID != nil {
		tsDAO := cdbm.NewTenantSiteDAO(dbSession)
		ts, err := tsDAO.GetByTenantIDAndSiteID(ctx, tx, tenant.ID, *siteID, []string{cdbm.SiteRelationName})
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				siteDAO := cdbm.NewSiteDAO(dbSession)
				site, err := siteDAO.GetByID(ctx, tx, *siteID, nil, false)
				if err != nil {
					return false, err
				}

				// Site-scoped: resolve the effective capability for the exact Site.
				providerID = &site.InfrastructureProviderID
			} else {
				return false, err
			}
		} else {
			if ts.Site != nil {
				providerID = &ts.Site.InfrastructureProviderID
			} else {
				return false, errors.New("failed to retrieve related Site for Tenant/Site association, DB error")
			}
			// This will ensure TenantAccount is ready for the Site.
			siteOverride = ts.Config.TargetedInstanceCreation
		}
	}

	if providerID != nil {
		taDAO := cdbm.NewTenantAccountDAO(dbSession)
		tas, _, err := taDAO.GetAll(ctx, tx, cdbm.TenantAccountFilterInput{
			InfrastructureProviderID: providerID,
			TenantIDs:                []uuid.UUID{tenant.ID},
			Statuses:                 []string{cdbm.TenantAccountStatusReady},
		}, cdbp.PageInput{Limit: cutil.GetPtr(1)}, nil)
		if err != nil {
			return false, err
		}

		if len(tas) == 0 {
			return false, nil
		}

		ta := tas[0]
		if siteOverride != nil {
			return *siteOverride, nil
		}

		return ta.Config.TargetedInstanceCreation, nil
	}

	return false, nil
}

// HasSiteAccess reports whether the Tenant has an Allocation or effective TargetedInstanceCreation on the Site.
func HasSiteAccess(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, tenant *cdbm.Tenant, siteID uuid.UUID) (bool, error) {
	count, err := cdbm.NewAllocationDAO(dbSession).GetCount(ctx, tx, cdbm.AllocationFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{siteID}})
	if err != nil || count > 0 {
		return count > 0, err
	}
	return HasTargetedInstanceCreation(ctx, tx, dbSession, tenant, &PrivilegeScope{SiteID: &siteID})
}
