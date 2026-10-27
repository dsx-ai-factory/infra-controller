// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"errors"

	"github.com/google/uuid"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// TenantPrivilegeScope narrows where a Tenant's TargetedInstanceCreation
// capability must be effective for a privilege check. For
// TenantHasTargetedInstanceCreation a non-nil scope is required and the fields
// select how the capability is resolved:
//
//   - SiteID set: resolve the effective capability for that exact Site.
//   - InfrastructureProviderID set: privileged if the Ready TenantAccount for
//     that Provider has TargetedInstanceCreation enabled globally.
//
// SiteID and InfrastructureProviderID must not be set together.
type TenantPrivilegeScope struct {
	InfrastructureProviderID *uuid.UUID
	SiteID                   *uuid.UUID
}

// TenantHasTargetedInstanceCreation reports whether the Tenant has the
// TargetedInstanceCreation capability enabled within the given scope. It is
// nil-safe so callers don't have to repeat the tenant nil check, and it
// deliberately does not read the deprecated tenant-level flag
// (tenant.Config.TargetedInstanceCreation), which is superseded by
// TenantAccount.config with per-site TenantSite.config overrides. See
// TenantPrivilegeScope for how scope selects the resolution strategy.
func TenantHasTargetedInstanceCreation(ctx context.Context, tx *db.Tx, dbSession *db.Session, tenant *Tenant, scope *TenantPrivilegeScope) (bool, error) {
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
		tsDAO := NewTenantSiteDAO(dbSession)
		ts, err := tsDAO.GetByTenantIDAndSiteID(ctx, tx, tenant.ID, *siteID, []string{SiteRelationName})
		if err != nil {
			if errors.Is(err, db.ErrDoesNotExist) {
				siteDAO := NewSiteDAO(dbSession)
				site, err := siteDAO.GetByID(ctx, tx, *siteID, nil, false, false)
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
		taDAO := NewTenantAccountDAO(dbSession)
		tas, _, err := taDAO.GetAll(ctx, tx, TenantAccountFilterInput{
			InfrastructureProviderID: providerID,
			TenantIDs:                []uuid.UUID{tenant.ID},
			Statuses:                 []string{TenantAccountStatusReady},
		}, paginator.PageInput{Limit: cutil.GetPtr(1)}, nil)
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

// TenantHasSiteAccess reports whether the Tenant has an Allocation or effective TargetedInstanceCreation on the Site.
func TenantHasSiteAccess(ctx context.Context, tx *db.Tx, dbSession *db.Session, tenant *Tenant, siteID uuid.UUID) (bool, error) {
	count, err := NewAllocationDAO(dbSession).GetCount(ctx, tx, AllocationFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{siteID}})
	if err != nil || count > 0 {
		return count > 0, err
	}
	return TenantHasTargetedInstanceCreation(ctx, tx, dbSession, tenant, &TenantPrivilegeScope{SiteID: &siteID})
}
