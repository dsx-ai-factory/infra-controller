// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"context"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"

	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
)

func testOrgEntityBuildUser(t *testing.T, dbSession *cdb.Session, org, displayName string, roles ...string) *cdbm.User {
	t.Helper()
	user, err := cdbm.NewUserDAO(dbSession).Create(context.Background(), nil, cdbm.UserCreateInput{
		StarfleetID: cutil.GetPtr(uuid.NewString()),
		OrgData: cdbm.OrgData{
			org: cdbm.Org{Name: org, DisplayName: displayName, Roles: roles},
		},
	})
	require.NoError(t, err)
	return user
}

func testOrgEntityBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name, status string, user *cdbm.User) *cdbm.Site {
	t.Helper()
	site := cdbm.TestBuildSite(t, dbSession, ip, name, user)
	_, err := dbSession.DB.NewUpdate().Model((*cdbm.Site)(nil)).Set("status = ?", status).Where("id = ?", site.ID).Exec(context.Background())
	require.NoError(t, err)
	site.Status = status
	return site
}

func TestOrgEntityResolver_Resolve(t *testing.T) {
	ctx := context.Background()
	dbSession := cdbu.TestInitDB(t)
	defer dbSession.Close()
	cdbm.TestSetupSchema(t, dbSession)

	ipDAO := cdbm.NewInfrastructureProviderDAO(dbSession)
	tnDAO := cdbm.NewTenantDAO(dbSession)
	taDAO := cdbm.NewTenantAccountDAO(dbSession)

	getProviders := func(t *testing.T, org string) []cdbm.InfrastructureProvider {
		ips, err := ipDAO.GetAllByOrg(ctx, nil, org, nil)
		require.NoError(t, err)
		return ips
	}
	getTenants := func(t *testing.T, org string) []cdbm.Tenant {
		tns, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		require.NoError(t, err)
		return tns
	}
	getTenantAccounts := func(t *testing.T, org string) []cdbm.TenantAccount {
		tas, _, err := taDAO.GetAll(ctx, nil, cdbm.TenantAccountFilterInput{TenantOrgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		require.NoError(t, err)
		return tas
	}
	assertReadyStatusDetail := func(t *testing.T, ta cdbm.TenantAccount) {
		sds, err := cdbm.NewStatusDetailDAO(dbSession).GetRecentByEntityIDs(ctx, nil, []string{ta.ID.String()}, 1)
		require.NoError(t, err)
		require.Len(t, sds, 1)
		assert.Equal(t, cdbm.TenantAccountStatusReady, sds[0].Status)
	}

	tests := []struct {
		name             string
		org              string
		displayName      string
		roles            []string
		isServiceAccount bool
		// setup prepares the org's DB entries and returns the Sites the resolver may start Tenant creation on.
		setup func(t *testing.T, org string, user *cdbm.User) []*cdbm.Site
		// resolvers is how many resolvers, each with its own cache, resolve the org concurrently.
		resolvers int
		// beforeSecondResolve, when set, runs after the first Resolve, then the same resolver resolves again.
		beforeSecondResolve func(t *testing.T, org string, oe *OrgEntity)
		wantCreateTenantOn  []string
		check               func(t *testing.T, org string, oe *OrgEntity)
	}{
		{
			name:  "user without a Provider or Tenant role gets no entities and creates none",
			org:   "no-role-org",
			roles: []string{"SOME_OTHER_ROLE"},
			check: func(t *testing.T, org string, oe *OrgEntity) {
				assert.Equal(t, &OrgEntity{}, oe)
				assert.Empty(t, getProviders(t, org))
				assert.Empty(t, getTenants(t, org))
			},
		},
		{
			name:        "Provider Viewer on a new org creates the Infrastructure Provider only",
			org:         "viewer-org",
			displayName: "Viewer Org",
			roles:       []string{authz.ProviderViewerRole},
			check: func(t *testing.T, org string, oe *OrgEntity) {
				require.NotNil(t, oe.InfrastructureProvider)
				assert.Equal(t, org, oe.InfrastructureProvider.Name)
				assert.Equal(t, "Viewer Org", *oe.InfrastructureProvider.OrgDisplayName)
				assert.Nil(t, oe.Tenant)
				assert.Len(t, getProviders(t, org), 1)
				assert.Empty(t, getTenants(t, org))
			},
		},
		{
			name:  "Tenant Admin on a new org creates the Tenant and links Tenant Accounts created beforehand",
			org:   "invited-tenant-org",
			roles: []string{authz.TenantAdminRole},
			setup: func(t *testing.T, org string, user *cdbm.User) []*cdbm.Site {
				ip := cdbm.TestBuildInfrastructureProvider(t, dbSession, "inviting-provider", "inviting-provider-org", user)
				_, err := taDAO.Create(ctx, nil, cdbm.TenantAccountCreateInput{
					AccountNumber:            cdbm.GenerateAccountNumber(),
					TenantOrg:                org,
					InfrastructureProviderID: ip.ID,
					Status:                   cdbm.TenantAccountStatusInvited,
					CreatedBy:                user.ID,
				})
				require.NoError(t, err)
				return nil
			},
			check: func(t *testing.T, org string, oe *OrgEntity) {
				require.NotNil(t, oe.Tenant)
				assert.Nil(t, oe.InfrastructureProvider)
				assert.Empty(t, getProviders(t, org))
				tas := getTenantAccounts(t, org)
				require.Len(t, tas, 1)
				require.NotNil(t, tas[0].TenantID)
				assert.Equal(t, oe.Tenant.ID, *tas[0].TenantID)
			},
		},
		{
			name:  "second Resolve on the same resolver returns the cached Tenant without reading the DB",
			org:   "cached-org",
			roles: []string{authz.TenantAdminRole},
			beforeSecondResolve: func(t *testing.T, org string, oe *OrgEntity) {
				_, err := tnDAO.Update(ctx, nil, cdbm.TenantUpdateInput{TenantID: oe.Tenant.ID, Name: cutil.GetPtr("renamed-in-db")})
				require.NoError(t, err)
			},
			check: func(t *testing.T, org string, oe *OrgEntity) {
				require.NotNil(t, oe.Tenant)
				assert.Equal(t, org, oe.Tenant.Name)
			},
		},
		{
			name:        "token with a new org display name updates the stored Infrastructure Provider",
			org:         "renamed-org",
			displayName: "New Name",
			roles:       []string{authz.ProviderAdminRole},
			setup: func(t *testing.T, org string, user *cdbm.User) []*cdbm.Site {
				cdbm.TestBuildInfrastructureProvider(t, dbSession, "renamed-provider", org, user)
				return nil
			},
			check: func(t *testing.T, org string, oe *OrgEntity) {
				require.NotNil(t, oe.InfrastructureProvider)
				assert.Equal(t, "New Name", *oe.InfrastructureProvider.OrgDisplayName)
				ips := getProviders(t, org)
				require.Len(t, ips, 1)
				assert.Equal(t, "New Name", *ips[0].OrgDisplayName)
			},
		},
		{
			name:  "token without an org display name keeps the stored one",
			org:   "unnamed-token-org",
			roles: []string{authz.TenantAdminRole},
			setup: func(t *testing.T, org string, user *cdbm.User) []*cdbm.Site {
				tn := cdbm.TestBuildTenant(t, dbSession, "unnamed-token-tenant", org, user)
				_, err := tnDAO.Update(ctx, nil, cdbm.TenantUpdateInput{TenantID: tn.ID, OrgDisplayName: cutil.GetPtr("Stored Name")})
				require.NoError(t, err)
				return nil
			},
			check: func(t *testing.T, org string, oe *OrgEntity) {
				require.NotNil(t, oe.Tenant)
				assert.Equal(t, "Stored Name", *oe.Tenant.OrgDisplayName)
				tns := getTenants(t, org)
				require.Len(t, tns, 1)
				assert.Equal(t, "Stored Name", *tns[0].OrgDisplayName)
			},
		},
		{
			name:             "service account on a new org creates the Infrastructure Provider, Tenant and a Ready Tenant Account",
			org:              "new-service-account-org",
			roles:            []string{authz.ProviderAdminRole, authz.TenantAdminRole},
			isServiceAccount: true,
			check: func(t *testing.T, org string, oe *OrgEntity) {
				assert.True(t, oe.IsServiceAccount)
				require.NotNil(t, oe.InfrastructureProvider)
				require.NotNil(t, oe.Tenant)
				tas := getTenantAccounts(t, org)
				require.Len(t, tas, 1)
				assert.Equal(t, oe.InfrastructureProvider.ID, tas[0].InfrastructureProviderID)
				require.NotNil(t, tas[0].TenantID)
				assert.Equal(t, oe.Tenant.ID, *tas[0].TenantID)
				assert.Equal(t, cdbm.TenantAccountStatusReady, tas[0].Status)
				assert.True(t, tas[0].Config.TargetedInstanceCreation)
				assertReadyStatusDetail(t, tas[0])
			},
		},
		{
			name:             "service account repairs its Tenant Account and starts Tenant creation on Registered Sites without a Tenant Site",
			org:              "repaired-service-account-org",
			roles:            []string{authz.ProviderAdminRole, authz.TenantAdminRole},
			isServiceAccount: true,
			setup: func(t *testing.T, org string, user *cdbm.User) []*cdbm.Site {
				ip := cdbm.TestBuildInfrastructureProvider(t, dbSession, "repaired-provider", org, user)
				tn := cdbm.TestBuildTenant(t, dbSession, "repaired-tenant", org, user)
				_, err := taDAO.Create(ctx, nil, cdbm.TenantAccountCreateInput{
					AccountNumber:            cdbm.GenerateAccountNumber(),
					TenantOrg:                org,
					InfrastructureProviderID: ip.ID,
					Status:                   cdbm.TenantAccountStatusPending,
					Config:                   &cdbm.TenantAccountConfig{CrossTenantInterfaceCreation: true},
					CreatedBy:                user.ID,
				})
				require.NoError(t, err)
				associated := testOrgEntityBuildSite(t, dbSession, ip, "associated-site", cdbm.SiteStatusRegistered, user)
				cdbm.TestBuildTenantSite(t, dbSession, tn, associated, nil, user)
				return []*cdbm.Site{
					testOrgEntityBuildSite(t, dbSession, ip, "registered-site", cdbm.SiteStatusRegistered, user),
					testOrgEntityBuildSite(t, dbSession, ip, "pending-site", cdbm.SiteStatusPending, user),
					associated,
				}
			},
			wantCreateTenantOn: []string{"registered-site"},
			check: func(t *testing.T, org string, oe *OrgEntity) {
				require.NotNil(t, oe.Tenant)
				tas := getTenantAccounts(t, org)
				require.Len(t, tas, 1)
				require.NotNil(t, tas[0].TenantID)
				assert.Equal(t, oe.Tenant.ID, *tas[0].TenantID)
				assert.Equal(t, cdbm.TenantAccountStatusReady, tas[0].Status)
				assert.True(t, tas[0].Config.TargetedInstanceCreation)
				assert.True(t, tas[0].Config.CrossTenantInterfaceCreation, "repair keeps the account's other capabilities")
				assertReadyStatusDetail(t, tas[0])
			},
		},
		{
			name:             "parallel first requests through separate resolvers create one of each entity",
			org:              "parallel-org",
			roles:            []string{authz.ProviderAdminRole, authz.TenantAdminRole},
			isServiceAccount: true,
			resolvers:        5,
			check: func(t *testing.T, org string, oe *OrgEntity) {
				assert.Len(t, getProviders(t, org), 1)
				assert.Len(t, getTenants(t, org), 1)
				assert.Len(t, getTenantAccounts(t, org), 1)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			user := testOrgEntityBuildUser(t, dbSession, tt.org, tt.displayName, tt.roles...)

			var sites []*cdbm.Site
			if tt.setup != nil {
				sites = tt.setup(t, tt.org, user)
			}
			scp := sc.NewClientPool(nil)
			siteClients := map[string]*tmocks.Client{}
			for _, site := range sites {
				stc := &tmocks.Client{}
				stc.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateTenant", mock.Anything).Return(&tmocks.WorkflowRun{}, nil).Maybe()
				scp.IDClientMap[site.ID.String()] = stc
				siteClients[site.Name] = stc
			}

			resolve := func(r *OrgEntityResolver) (*OrgEntity, error) {
				return r.Resolve(ctx, zerolog.Nop(), tt.org, user, tt.isServiceAccount)
			}

			var oe *OrgEntity
			if tt.resolvers > 1 {
				results := make([]*OrgEntity, tt.resolvers)
				errs := make([]error, tt.resolvers)
				var wg sync.WaitGroup
				for i := range tt.resolvers {
					wg.Add(1)
					go func() {
						defer wg.Done()
						results[i], errs[i] = resolve(NewOrgEntityResolver(dbSession, scp))
					}()
				}
				wg.Wait()
				for i := range tt.resolvers {
					require.NoError(t, errs[i])
					assert.Equal(t, results[0].Tenant.ID, results[i].Tenant.ID)
					assert.Equal(t, results[0].InfrastructureProvider.ID, results[i].InfrastructureProvider.ID)
				}
				oe = results[0]
			} else {
				resolver := NewOrgEntityResolver(dbSession, scp)
				var err error
				oe, err = resolve(resolver)
				require.NoError(t, err)
				if tt.beforeSecondResolve != nil {
					tt.beforeSecondResolve(t, tt.org, oe)
					oe, err = resolve(resolver)
					require.NoError(t, err)
				}
			}

			for name, stc := range siteClients {
				wantCalls := 0
				for _, want := range tt.wantCreateTenantOn {
					if want == name {
						wantCalls = 1
					}
				}
				stc.AssertNumberOfCalls(t, "ExecuteWorkflow", wantCalls)
			}
			tt.check(t, tt.org, oe)
		})
	}
}

func TestOrgEntityMiddleware(t *testing.T) {
	dbSession := cdbu.TestInitDB(t)
	defer dbSession.Close()
	cdbm.TestSetupSchema(t, dbSession)

	org := "middleware-org"
	user := testOrgEntityBuildUser(t, dbSession, org, "", authz.TenantAdminRole)

	tests := []struct {
		name           string
		orgParam       string
		user           *cdbm.User
		wantStatusCode int
		wantNextCalled bool
	}{
		{
			name:           "stores the OrgEntity of the lowercased path org for the next handler",
			orgParam:       "Middleware-Org",
			user:           user,
			wantStatusCode: http.StatusOK,
			wantNextCalled: true,
		},
		{
			name:           "responds 500 without calling the next handler when org entities can't be resolved",
			orgParam:       org,
			wantStatusCode: http.StatusInternalServerError,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := httptest.NewRecorder()
			c := echo.New().NewContext(httptest.NewRequest(http.MethodGet, "/", nil), rec)
			c.SetParamNames("orgName")
			c.SetParamValues(tt.orgParam)
			if tt.user != nil {
				c.Set("user", tt.user)
			}

			var nextOrgEntity *OrgEntity
			nextCalled := false
			next := func(c echo.Context) error {
				nextCalled = true
				nextOrgEntity = GetOrgEntity(c.Request().Context())
				return c.NoContent(http.StatusOK)
			}

			err := OrgEntityMiddleware(NewOrgEntityResolver(dbSession, sc.NewClientPool(nil)))(next)(c)
			require.NoError(t, err)
			assert.Equal(t, tt.wantStatusCode, rec.Code)
			assert.Equal(t, tt.wantNextCalled, nextCalled)
			if tt.wantNextCalled {
				require.NotNil(t, nextOrgEntity)
				require.NotNil(t, nextOrgEntity.Tenant)
				assert.Equal(t, org, nextOrgEntity.Tenant.Org)
			}
		})
	}
}
