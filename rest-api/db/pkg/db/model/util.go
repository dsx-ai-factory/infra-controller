// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"errors"

	mapset "github.com/deckarep/golang-set/v2"
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

	ta, siteConfig, err := readyTenantAccountForScope(ctx, tx, dbSession, tenant.ID, scope)
	if err != nil || ta == nil {
		return false, err
	}
	if siteConfig != nil && siteConfig.TargetedInstanceCreation != nil {
		return *siteConfig.TargetedInstanceCreation, nil
	}

	return ta.Config.TargetedInstanceCreation, nil
}

// TenantHasCrossTenantInterfaceCreation reports whether the Tenant may create or
// retarget an Interface to another Tenant's VPC or VPC Prefix at the specified Site.
func TenantHasCrossTenantInterfaceCreation(ctx context.Context, tx *db.Tx, dbSession *db.Session, tenant *Tenant, siteID uuid.UUID) (bool, error) {
	if tenant == nil {
		return false, nil
	}

	ta, siteConfig, err := readyTenantAccountForScope(ctx, tx, dbSession, tenant.ID, &TenantPrivilegeScope{SiteID: &siteID})
	if err != nil || ta == nil {
		return false, err
	}
	if siteConfig != nil && siteConfig.CrossTenantInterfaceCreation != nil {
		return *siteConfig.CrossTenantInterfaceCreation, nil
	}

	return ta.Config.CrossTenantInterfaceCreation, nil
}

// readyTenantAccountForScope resolves the Ready TenantAccount and optional
// TenantSite configuration that own a provider- or Site-scoped capability.
func readyTenantAccountForScope(ctx context.Context, tx *db.Tx, dbSession *db.Session, tenantID uuid.UUID, scope *TenantPrivilegeScope) (*TenantAccount, *TenantSiteConfig, error) {
	siteID := scope.SiteID
	providerID := scope.InfrastructureProviderID
	var siteConfig *TenantSiteConfig

	if siteID != nil {
		ts, err := NewTenantSiteDAO(dbSession).GetByTenantIDAndSiteID(ctx, tx, tenantID, *siteID, []string{SiteRelationName})
		if err != nil {
			if errors.Is(err, db.ErrDoesNotExist) {
				site, siteErr := NewSiteDAO(dbSession).GetByID(ctx, tx, *siteID, nil, false, false)
				if siteErr != nil {
					return nil, nil, siteErr
				}
				providerID = &site.InfrastructureProviderID
			} else {
				return nil, nil, err
			}
		} else {
			if ts.Site == nil {
				return nil, nil, errors.New("failed to retrieve related Site for Tenant/Site association, DB error")
			}
			providerID = &ts.Site.InfrastructureProviderID
			siteConfig = &ts.Config
		}
	}

	if providerID == nil {
		return nil, siteConfig, nil
	}

	tas, _, err := NewTenantAccountDAO(dbSession).GetAll(ctx, tx, TenantAccountFilterInput{
		InfrastructureProviderID: providerID,
		TenantIDs:                []uuid.UUID{tenantID},
		Statuses:                 []string{TenantAccountStatusReady},
	}, paginator.PageInput{Limit: cutil.GetPtr(1)}, nil)
	if err != nil {
		return nil, nil, err
	}
	if len(tas) == 0 {
		return nil, siteConfig, nil
	}

	return &tas[0], siteConfig, nil
}

// TenantHasSiteAccess reports whether the Tenant has an Allocation or effective TargetedInstanceCreation on the Site.
func TenantHasSiteAccess(ctx context.Context, tx *db.Tx, dbSession *db.Session, tenant *Tenant, siteID uuid.UUID) (bool, error) {
	count, err := NewAllocationDAO(dbSession).GetCount(ctx, tx, AllocationFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{siteID}})
	if err != nil || count > 0 {
		return count > 0, err
	}
	return TenantHasTargetedInstanceCreation(ctx, tx, dbSession, tenant, &TenantPrivilegeScope{SiteID: &siteID})
}

// GetPrivilegedAccessSiteIDsForTenant returns Site IDs where the Tenant has
// effective TargetedInstanceCreation via a Ready TenantAccount and optional
// TenantSite.config overrides. It is nil-safe and returns an empty slice when
// the Tenant has no privileged Site access.
func GetPrivilegedAccessSiteIDsForTenant(ctx context.Context, tx *db.Tx, dbSession *db.Session, tenant *Tenant) ([]uuid.UUID, error) {
	if tenant == nil {
		return nil, nil
	}

	tas, _, err := NewTenantAccountDAO(dbSession).GetAll(ctx, tx, TenantAccountFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
		Statuses:  []string{TenantAccountStatusReady},
	}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	if err != nil {
		return nil, err
	}
	if len(tas) == 0 {
		return nil, nil
	}

	readyProviderIDs := mapset.NewSet[uuid.UUID]()
	enabledProviderIDs := mapset.NewSet[uuid.UUID]()
	for _, ta := range tas {
		readyProviderIDs.Add(ta.InfrastructureProviderID)
		if ta.Config.TargetedInstanceCreation {
			enabledProviderIDs.Add(ta.InfrastructureProviderID)
		}
	}

	siteIDs := mapset.NewSet[uuid.UUID]()
	if !enabledProviderIDs.IsEmpty() {
		sites, _, err := NewSiteDAO(dbSession).GetAll(ctx, tx, SiteFilterInput{
			InfrastructureProviderIDs: enabledProviderIDs.ToSlice(),
		}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
		if err != nil {
			return nil, err
		}

		for _, site := range sites {
			siteIDs.Add(site.ID)
		}
	}

	tss, _, err := NewTenantSiteDAO(dbSession).GetAll(ctx, tx, TenantSiteFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
	}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, []string{SiteRelationName})
	if err != nil {
		return nil, err
	}

	for _, ts := range tss {
		if ts.Config.TargetedInstanceCreation != nil {
			if *ts.Config.TargetedInstanceCreation {
				if ts.Site != nil && readyProviderIDs.Contains(ts.Site.InfrastructureProviderID) {
					siteIDs.Add(ts.SiteID)
				}
			} else {
				siteIDs.Remove(ts.SiteID)
			}
		}
	}

	return siteIDs.ToSlice(), nil
}
