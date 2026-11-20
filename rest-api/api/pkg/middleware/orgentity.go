// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"time"

	mapset "github.com/deckarep/golang-set/v2"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"golang.org/x/sync/singleflight"

	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// orgEntityInitTimeout bounds the shared initialization a burst of first requests waits on.
const orgEntityInitTimeout = 30 * time.Second

// OrgEntity carries the org's Infrastructure Provider and Tenant resolved for the request user's roles.
// InfrastructureProvider is set for Provider Admins and Viewers, Tenant for Tenant Admins.
type OrgEntity struct {
	InfrastructureProvider *cdbm.InfrastructureProvider
	Tenant                 *cdbm.Tenant
	IsServiceAccount       bool
}

type orgEntityContextKey struct{}

// WithOrgEntity returns a copy of ctx that carries oe.
func WithOrgEntity(ctx context.Context, oe *OrgEntity) context.Context {
	return context.WithValue(ctx, orgEntityContextKey{}, oe)
}

// GetOrgEntity returns the OrgEntity stored by OrgEntityMiddleware, or nil when the middleware did not run.
func GetOrgEntity(ctx context.Context) *OrgEntity {
	oe, _ := ctx.Value(orgEntityContextKey{}).(*OrgEntity)
	return oe
}

// orgEntityNeeds is what a request needs resolved for its org, derived from the user's roles.
type orgEntityNeeds struct {
	provider       bool
	tenant         bool
	serviceAccount bool
}

// orgEntityState is what the DB holds for an org while it is being resolved.
type orgEntityState struct {
	infrastructureProvider      *cdbm.InfrastructureProvider
	tenant                      *cdbm.Tenant
	serviceAccountTenantAccount *cdbm.TenantAccount
	// serviceAccountRepaired is set when the service account TenantAccount was created or made Ready.
	serviceAccountRepaired bool
}

// orgEntityCacheEntry is what has been resolved for an org on this pod.
type orgEntityCacheEntry struct {
	infrastructureProvider *cdbm.InfrastructureProvider
	tenant                 *cdbm.Tenant
	// serviceAccountReady is set once the service account TenantAccount has been checked on this pod.
	serviceAccountReady bool
}

// OrgEntityResolver gets or creates the org's Infrastructure Provider and Tenant, and caches them for the pod's
// lifetime. The API never deletes either entity, so a cached entry can only go stale on its org display name,
// which every request compares against the token.
type OrgEntityResolver struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	mu        sync.RWMutex
	cache     map[string]*orgEntityCacheEntry
	inits     singleflight.Group
}

// NewOrgEntityResolver initializes and returns a new OrgEntityResolver with an empty cache
func NewOrgEntityResolver(dbSession *cdb.Session, scp *sc.ClientPool) *OrgEntityResolver {
	return &OrgEntityResolver{
		dbSession: dbSession,
		scp:       scp,
		cache:     map[string]*orgEntityCacheEntry{},
	}
}

// Resolve returns the org's entities that the user's roles grant. A Provider Admin or Viewer gets the
// Infrastructure Provider, a Tenant Admin gets the Tenant, and either is created when missing. A service account,
// which holds both roles, also gets a Ready TenantAccount between the two with TargetedInstanceCreation enabled.
func (r *OrgEntityResolver) Resolve(ctx context.Context, logger zerolog.Logger, org string, user *cdbm.User, isServiceAccount bool) (*OrgEntity, error) {
	needs := orgEntityNeeds{
		provider: authz.ValidateUserRoles(user, org, nil, authz.ProviderAdminRole, authz.ProviderViewerRole),
		tenant:   authz.ValidateUserRoles(user, org, nil, authz.TenantAdminRole),
	}
	needs.serviceAccount = isServiceAccount && needs.provider && needs.tenant
	if !needs.provider && !needs.tenant {
		return &OrgEntity{}, nil
	}

	name := org
	displayName := ""
	userOrg, err := user.OrgData.GetOrgByName(org)
	if err == nil {
		if userOrg.Name != "" {
			name = userOrg.Name
		}
		displayName = userOrg.DisplayName
	}

	r.mu.RLock()
	entry := r.cache[org]
	r.mu.RUnlock()

	if !entry.satisfies(needs, displayName) {
		key := fmt.Sprintf("%s/%t/%t/%t", org, needs.provider, needs.tenant, needs.serviceAccount)
		v, err, _ := r.inits.Do(key, func() (any, error) {
			// Runs without the caller's cancellation, so one caller disconnecting doesn't fail the others waiting on it.
			ictx, cancel := context.WithTimeout(context.WithoutCancel(ctx), orgEntityInitTimeout)
			defer cancel()
			return r.initialize(ictx, logger, org, name, displayName, user.ID, needs)
		})
		if err != nil {
			return nil, err
		}
		entry = v.(*orgEntityCacheEntry)
	}

	return entry.orgEntity(needs), nil
}

func (r *OrgEntityResolver) initialize(ctx context.Context, logger zerolog.Logger, org, name, displayName string, createdBy uuid.UUID, needs orgEntityNeeds) (*orgEntityCacheEntry, error) {
	// An org that is already set up, such as on the first request after a pod starts, needs no lock.
	state, err := r.load(ctx, nil, org, needs)
	if err != nil {
		return nil, err
	}
	if !state.needsWrite(needs, displayName) {
		return r.store(org, state, needs), nil
	}

	state, err = cdb.WithTxResult(ctx, r.dbSession, func(tx *cdb.Tx) (*orgEntityState, error) {
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString("org-entities-"+org), nil)
		if derr != nil {
			return nil, fmt.Errorf("failed to acquire org entity lock: %w", derr)
		}
		locked, derr := r.load(ctx, tx, org, needs)
		if derr != nil {
			return nil, derr
		}
		derr = r.write(ctx, tx, org, name, displayName, createdBy, needs, locked)
		if derr != nil {
			return nil, derr
		}
		return locked, nil
	})
	if err != nil {
		return nil, err
	}
	logger.Info().Bool("Service Account", needs.serviceAccount).Msg("created or updated org's Infrastructure Provider/Tenant")

	if state.serviceAccountRepaired {
		r.startServiceAccountTenantCreates(ctx, logger, state.tenant)
	}

	return r.store(org, state, needs), nil
}

func (r *OrgEntityResolver) load(ctx context.Context, tx *cdb.Tx, org string, needs orgEntityNeeds) (*orgEntityState, error) {
	state := &orgEntityState{}

	if needs.provider {
		ips, err := cdbm.NewInfrastructureProviderDAO(r.dbSession).GetAllByOrg(ctx, tx, org, nil)
		if err != nil {
			return nil, fmt.Errorf("failed to retrieve Infrastructure Provider for org: %w", err)
		}
		if len(ips) > 0 {
			state.infrastructureProvider = &ips[0]
		}
	}

	if needs.tenant {
		tns, _, err := cdbm.NewTenantDAO(r.dbSession).GetAll(ctx, tx, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			return nil, fmt.Errorf("failed to retrieve Tenant for org: %w", err)
		}
		if len(tns) > 0 {
			state.tenant = &tns[0]
		}
	}

	if needs.serviceAccount && state.infrastructureProvider != nil {
		tas, _, err := cdbm.NewTenantAccountDAO(r.dbSession).GetAll(ctx, tx, cdbm.TenantAccountFilterInput{
			InfrastructureProviderID: &state.infrastructureProvider.ID,
			TenantOrgs:               []string{org},
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			return nil, fmt.Errorf("failed to retrieve service account Tenant Account for org: %w", err)
		}
		if len(tas) > 0 {
			state.serviceAccountTenantAccount = &tas[0]
		}
	}

	return state, nil
}

func (s *orgEntityState) needsWrite(needs orgEntityNeeds, displayName string) bool {
	if needs.provider && (s.infrastructureProvider == nil || orgDisplayNameStale(s.infrastructureProvider.OrgDisplayName, displayName)) {
		return true
	}
	if needs.tenant && (s.tenant == nil || orgDisplayNameStale(s.tenant.OrgDisplayName, displayName)) {
		return true
	}
	return needs.serviceAccount && !serviceAccountTenantAccountReady(s.serviceAccountTenantAccount, s.tenant)
}

func (r *OrgEntityResolver) write(ctx context.Context, tx *cdb.Tx, org, name, displayName string, createdBy uuid.UUID, needs orgEntityNeeds, s *orgEntityState) error {
	orgDisplayName := displayName
	if orgDisplayName == "" {
		orgDisplayName = org
	}

	if needs.provider {
		ipDAO := cdbm.NewInfrastructureProviderDAO(r.dbSession)
		if s.infrastructureProvider == nil {
			ip, err := ipDAO.Create(ctx, tx, cdbm.InfrastructureProviderCreateInput{
				Name:           name,
				Org:            org,
				OrgDisplayName: &orgDisplayName,
				CreatedBy:      createdBy,
			})
			if err != nil {
				return fmt.Errorf("failed to create Infrastructure Provider for org: %w", err)
			}
			s.infrastructureProvider = ip
		} else if orgDisplayNameStale(s.infrastructureProvider.OrgDisplayName, displayName) {
			ip, err := ipDAO.Update(ctx, tx, cdbm.InfrastructureProviderUpdateInput{
				InfrastructureProviderID: s.infrastructureProvider.ID,
				OrgDisplayName:           &displayName,
			})
			if err != nil {
				return fmt.Errorf("failed to update Infrastructure Provider org display name: %w", err)
			}
			s.infrastructureProvider = ip
		}
	}

	if needs.tenant {
		tnDAO := cdbm.NewTenantDAO(r.dbSession)
		if s.tenant == nil {
			tn, err := tnDAO.Create(ctx, tx, cdbm.TenantCreateInput{
				Name:           name,
				DisplayName:    &orgDisplayName,
				Org:            org,
				OrgDisplayName: &orgDisplayName,
				CreatedBy:      createdBy,
			})
			if err != nil {
				return fmt.Errorf("failed to create Tenant for org: %w", err)
			}
			err = r.linkTenantAccounts(ctx, tx, tn)
			if err != nil {
				return err
			}
			s.tenant = tn
		} else if orgDisplayNameStale(s.tenant.OrgDisplayName, displayName) {
			tn, err := tnDAO.Update(ctx, tx, cdbm.TenantUpdateInput{
				TenantID:       s.tenant.ID,
				OrgDisplayName: &displayName,
			})
			if err != nil {
				return fmt.Errorf("failed to update Tenant org display name: %w", err)
			}
			s.tenant = tn
		}
	}

	if needs.serviceAccount && !serviceAccountTenantAccountReady(s.serviceAccountTenantAccount, s.tenant) {
		err := r.ensureServiceAccountTenantAccount(ctx, tx, org, createdBy, s)
		if err != nil {
			return err
		}
		s.serviceAccountRepaired = true
	}

	return nil
}

// linkTenantAccounts sets the new Tenant's ID on the TenantAccounts Providers created for its org beforehand.
func (r *OrgEntityResolver) linkTenantAccounts(ctx context.Context, tx *cdb.Tx, tenant *cdbm.Tenant) error {
	taDAO := cdbm.NewTenantAccountDAO(r.dbSession)
	tas, _, err := taDAO.GetAll(ctx, tx, cdbm.TenantAccountFilterInput{TenantOrgs: []string{tenant.Org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return fmt.Errorf("failed to retrieve Tenant Accounts for org: %w", err)
	}
	for _, ta := range tas {
		_, err = taDAO.Update(ctx, tx, cdbm.TenantAccountUpdateInput{
			TenantAccountID: ta.ID,
			TenantID:        &tenant.ID,
		})
		if err != nil {
			return fmt.Errorf("failed to set Tenant ID on Tenant Account %s: %w", ta.ID, err)
		}
	}
	return nil
}

func (r *OrgEntityResolver) ensureServiceAccountTenantAccount(ctx context.Context, tx *cdb.Tx, org string, createdBy uuid.UUID, s *orgEntityState) error {
	taDAO := cdbm.NewTenantAccountDAO(r.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(r.dbSession)
	ip := s.infrastructureProvider
	tn := s.tenant
	readyMessage := cutil.GetPtr("service account enabled, tenant account ready")

	ta := s.serviceAccountTenantAccount
	if ta == nil {
		created, err := taDAO.Create(ctx, tx, cdbm.TenantAccountCreateInput{
			AccountNumber:             cdbm.GenerateAccountNumber(),
			TenantID:                  &tn.ID,
			TenantOrg:                 org,
			InfrastructureProviderID:  ip.ID,
			InfrastructureProviderOrg: ip.Org,
			Status:                    cdbm.TenantAccountStatusReady,
			Config:                    &cdbm.TenantAccountConfig{TargetedInstanceCreation: true},
			CreatedBy:                 createdBy,
		})
		if err != nil {
			return fmt.Errorf("failed to create service account Tenant Account: %w", err)
		}
		_, err = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: created.ID.String(),
			Status:   cdbm.TenantAccountStatusReady,
			Message:  readyMessage,
		})
		if err != nil {
			return fmt.Errorf("failed to create Status Detail for service account Tenant Account: %w", err)
		}
		s.serviceAccountTenantAccount = created
		return nil
	}

	accountConfig := ta.Config
	accountConfig.TargetedInstanceCreation = true
	updated, err := taDAO.Update(ctx, tx, cdbm.TenantAccountUpdateInput{
		TenantAccountID: ta.ID,
		TenantID:        &tn.ID,
		Status:          cutil.GetPtr(cdbm.TenantAccountStatusReady),
		Config:          &accountConfig,
	})
	if err != nil {
		return fmt.Errorf("failed to update service account Tenant Account: %w", err)
	}
	if ta.Status != cdbm.TenantAccountStatusReady {
		_, err = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
			EntityID: ta.ID.String(),
			Status:   cdbm.TenantAccountStatusReady,
			Message:  readyMessage,
		})
		if err != nil {
			return fmt.Errorf("failed to create Status Detail for service account Tenant Account: %w", err)
		}
	}
	s.serviceAccountTenantAccount = updated
	return nil
}

// startServiceAccountTenantCreates triggers Tenant creation on the service account's Registered privileged Sites.
// A Site with a TenantSite is skipped, since creating the TenantSite already triggered it. Failures are logged,
// and inventory reconciliation retries them.
func (r *OrgEntityResolver) startServiceAccountTenantCreates(ctx context.Context, logger zerolog.Logger, tenant *cdbm.Tenant) {
	siteIDs, err := cdbm.GetPrivilegedAccessSiteIDsForTenant(ctx, nil, r.dbSession, tenant)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to retrieve privileged Sites for service account Tenant creation")
		return
	}
	if len(siteIDs) == 0 {
		return
	}

	tss, _, err := cdbm.NewTenantSiteDAO(r.dbSession).GetAll(ctx, nil, cdbm.TenantSiteFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
		SiteIDs:   siteIDs,
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to retrieve Tenant Sites for service account Tenant creation")
		return
	}
	missingSiteIDs := mapset.NewSet(siteIDs...)
	for _, ts := range tss {
		missingSiteIDs.Remove(ts.SiteID)
	}
	if missingSiteIDs.IsEmpty() {
		return
	}

	sites, _, err := cdbm.NewSiteDAO(r.dbSession).GetAll(ctx, nil, cdbm.SiteFilterInput{
		SiteIDs:  missingSiteIDs.ToSlice(),
		Statuses: []string{cdbm.SiteStatusRegistered},
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to retrieve Registered Sites for service account Tenant creation")
		return
	}
	for _, site := range sites {
		r.scp.StartTenantCreate(ctx, logger, tenant, site.ID)
	}
}

// store merges state into the org's cache entry and returns the merged entry. Entities a request didn't need are
// kept from the previous entry, and a missing entity is never cached.
func (r *OrgEntityResolver) store(org string, state *orgEntityState, needs orgEntityNeeds) *orgEntityCacheEntry {
	r.mu.Lock()
	defer r.mu.Unlock()

	next := &orgEntityCacheEntry{}
	previous := r.cache[org]
	if previous != nil {
		*next = *previous
	}
	if state.infrastructureProvider != nil {
		next.infrastructureProvider = state.infrastructureProvider
	}
	if state.tenant != nil {
		next.tenant = state.tenant
	}
	if needs.serviceAccount {
		next.serviceAccountReady = true
	}
	r.cache[org] = next
	return next
}

func (e *orgEntityCacheEntry) satisfies(needs orgEntityNeeds, displayName string) bool {
	if e == nil {
		return false
	}
	if needs.provider && (e.infrastructureProvider == nil || orgDisplayNameStale(e.infrastructureProvider.OrgDisplayName, displayName)) {
		return false
	}
	if needs.tenant && (e.tenant == nil || orgDisplayNameStale(e.tenant.OrgDisplayName, displayName)) {
		return false
	}
	return !needs.serviceAccount || e.serviceAccountReady
}

// orgEntity returns copies of the cached entities, so a handler that changes one can't change the cache.
func (e *orgEntityCacheEntry) orgEntity(needs orgEntityNeeds) *OrgEntity {
	oe := &OrgEntity{IsServiceAccount: needs.serviceAccount}
	if needs.provider && e.infrastructureProvider != nil {
		ip := *e.infrastructureProvider
		oe.InfrastructureProvider = &ip
	}
	if needs.tenant && e.tenant != nil {
		tn := *e.tenant
		oe.Tenant = &tn
	}
	return oe
}

// orgDisplayNameStale reports whether the token carries an org display name the entity doesn't have yet.
// An empty display name never replaces a stored one, since some token issuers omit it.
func orgDisplayNameStale(stored *string, displayName string) bool {
	return displayName != "" && (stored == nil || *stored != displayName)
}

func serviceAccountTenantAccountReady(ta *cdbm.TenantAccount, tenant *cdbm.Tenant) bool {
	return ta != nil && tenant != nil && ta.Status == cdbm.TenantAccountStatusReady && ta.Config.TargetedInstanceCreation &&
		ta.TenantID != nil && *ta.TenantID == tenant.ID
}

// resolveOrgEntity resolves the entities for the org in the request path and stores them in the request context.
func resolveOrgEntity(c echo.Context, resolver *OrgEntityResolver, logger zerolog.Logger) (*OrgEntity, error) {
	org := strings.ToLower(c.Param("orgName"))
	user, ok := c.Get("user").(*cdbm.User)
	if org == "" || !ok || user == nil {
		return nil, errors.New("request has no org or user to resolve org entities for")
	}

	oe, err := resolver.Resolve(c.Request().Context(), logger.With().Str("Org", org).Logger(), org, user, cauth.GetIsServiceAccountFromContext(c))
	if err != nil {
		return nil, err
	}
	c.SetRequest(c.Request().WithContext(WithOrgEntity(c.Request().Context(), oe)))
	return oe, nil
}

// OrgEntityMiddleware stores the request org's OrgEntity in the request context, creating the org's
// Infrastructure Provider and Tenant on the first request from a user whose roles grant them. It never rejects a
// request based on roles, handlers keep their own authorization. It must run after authentication.
func OrgEntityMiddleware(resolver *OrgEntityResolver) echo.MiddlewareFunc {
	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			logger := log.With().Str("Middleware", "OrgEntity").Logger()
			_, err := resolveOrgEntity(c, resolver, logger)
			if err != nil {
				logger.Error().Err(err).Msg("failed to resolve org's Infrastructure Provider/Tenant")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to initialize org's Infrastructure Provider/Tenant", nil)
			}
			return next(c)
		}
	}
}
