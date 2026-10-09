// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

func TestServiceAccountHandler_GetCurrent(t *testing.T) {
	ctx := context.Background()

	// Initialize test database
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	org1 := "test-org"
	user1 := common.TestBuildUser(t, dbSession, uuid.NewString(), org1, []string{authz.ProviderAdminRole, authz.TenantAdminRole})

	org2 := "test-org-2"
	user2 := common.TestBuildUser(t, dbSession, uuid.NewString(), org2, []string{authz.ProviderAdminRole, authz.TenantAdminRole})

	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "test-provider-2", org2, user2)
	tn2 := common.TestBuildTenant(t, dbSession, "test-tenant-2", org2, user2)
	_ = common.TestBuildTenantAccount(t, dbSession, ip2, &tn2.ID, org2, cdbm.TenantAccountStatusReady, user2)

	org3 := "test-org-3"
	user3 := common.TestBuildUser(t, dbSession, uuid.NewString(), org3, []string{authz.TenantAdminRole})

	org4 := "test-org-4"
	user4 := common.TestBuildUser(t, dbSession, uuid.NewString(), org4, []string{authz.ProviderAdminRole, authz.TenantAdminRole})
	ip4 := common.TestBuildInfrastructureProvider(t, dbSession, "test-provider-4", org4, user4)
	tn4 := common.TestBuildTenant(t, dbSession, "test-tenant-4", org4, user4)
	ta4 := common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip4, &tn4.ID, org4, cdbm.TenantAccountStatusPending, user4)
	sites := map[string]*cdbm.Site{
		org2: testIPBlockBuildSite(t, dbSession, ip2, "existing-account-site", cdbm.SiteStatusRegistered, false, user2),
		org4: testIPBlockBuildSite(t, dbSession, ip4, "promoted-account-site", cdbm.SiteStatusRegistered, false, user4),
	}

	tests := []struct {
		name                  string
		org                   string
		user                  *cdbm.User
		serviceAccountEnabled bool
	}{
		{
			name:                  "test get current ServiceAccount when service account is enabled and org doesn't have Provider/Tenant/TenantAccount",
			org:                   org1,
			user:                  user1,
			serviceAccountEnabled: true,
		},
		{
			name:                  "test get current ServiceAccount tolerates association lock failure and retries",
			org:                   org2,
			user:                  user2,
			serviceAccountEnabled: true,
		},
		{
			name:                  "test get current ServiceAccount when service account is disabled",
			org:                   org3,
			user:                  user3,
			serviceAccountEnabled: false,
		},
		{
			name:                  "test get current ServiceAccount promotes existing capable TenantAccount to Ready",
			org:                   org4,
			user:                  user4,
			serviceAccountEnabled: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			site := sites[test.org]
			scp := sc.NewClientPool(nil)
			tsc := &tmocks.Client{}
			if site != nil {
				scp.IDClientMap[site.ID.String()] = tsc
				tsc.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateTenant", &corev1.CreateTenantRequest{
					OrganizationId: test.org, Metadata: &corev1.Metadata{Name: "Test Tenant"},
				}).Return(&tmocks.WorkflowRun{}, nil).Once()
			}
			defer tsc.AssertExpectations(t)

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/service-account/current", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(test.org)
			ec.Set("user", test.user)

			ec.SetRequest(ec.Request().WithContext(ctx))

			// Normally, the auth processor records the service-account flag on the request
			// context based on the type of issuer/Origin/claimMappings, but in this test we
			// set it manually for testing purposes.
			cauth.SetIsServiceAccountInContext(ec, test.serviceAccountEnabled)

			handler := NewGetCurrentServiceAccountHandler(dbSession, scp)
			if test.org == org2 {
				lockKey := fmt.Sprintf("%s-%s-%s", ip2.ID, site.ID, tn2.ID)
				testWithAdvisoryLocks(t, ctx, dbSession, []string{lockKey}, func(_ *cdb.Tx) error {
					err := handler.Handle(ec)
					assert.Equal(t, http.StatusOK, rec.Code)
					assert.JSONEq(t, fmt.Sprintf(`{"enabled":true,"infrastructureProviderId":%q,"tenantId":%q}`, ip2.ID.String(), tn2.ID.String()), rec.Body.String())
					tsc.AssertNumberOfCalls(t, "ExecuteWorkflow", 0)
					_, serr := cdbm.NewTenantSiteDAO(dbSession).GetByTenantIDAndSiteID(ctx, nil, tn2.ID, site.ID, nil)
					assert.ErrorIs(t, serr, cdb.ErrDoesNotExist)
					return err
				})
				rec = httptest.NewRecorder()
				ec.SetResponse(echo.NewResponse(rec, e))
			}

			err := handler.Handle(ec)
			require.NoError(t, err)
			require.Equal(t, http.StatusOK, rec.Code)

			sa := &model.APIServiceAccount{}
			err = json.Unmarshal(rec.Body.Bytes(), sa)
			require.NoError(t, err)

			assert.Equal(t, test.serviceAccountEnabled, sa.Enabled)
			if site != nil {
				require.NotNil(t, sa.TenantID)
				ts, serr := cdbm.NewTenantSiteDAO(dbSession).GetByTenantIDAndSiteID(ctx, nil, uuid.MustParse(*sa.TenantID), site.ID, nil)
				require.NoError(t, serr)
				assert.Equal(t, test.user.ID, ts.CreatedBy)
			}

			if test.serviceAccountEnabled {
				assert.NotNil(t, sa.InfrastructureProviderID)
				assert.NotNil(t, sa.TenantID)
			} else {
				assert.Nil(t, sa.InfrastructureProviderID)
				assert.Nil(t, sa.TenantID)
			}

			// For the org with no pre-existing Tenant Account (org1), the handler
			// creates one and must record an initial Ready status detail so the
			// account's status history is never empty.
			if test.org == org1 {
				ipDAO := cdbm.NewInfrastructureProviderDAO(dbSession)
				tnDAO := cdbm.NewTenantDAO(dbSession)
				taDAO := cdbm.NewTenantAccountDAO(dbSession)
				sdDAO := cdbm.NewStatusDetailDAO(dbSession)

				ips, ipErr := ipDAO.GetAllByOrg(ctx, nil, org1, nil)
				require.NoError(t, ipErr)
				require.Len(t, ips, 1)
				tns, _, tnErr := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org1}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
				require.NoError(t, tnErr)
				require.Len(t, tns, 1)

				tas, _, taErr := taDAO.GetAll(ctx, nil, cdbm.TenantAccountFilterInput{
					InfrastructureProviderID: &ips[0].ID,
					TenantIDs:                []uuid.UUID{tns[0].ID},
				}, cdbp.PageInput{}, nil)
				require.NoError(t, taErr)
				require.Len(t, tas, 1)

				sds, sdErr := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{tas[0].ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
				require.NoError(t, sdErr)
				require.NotEmpty(t, sds, "service-account-created tenant account should have a status detail")
				assert.Equal(t, cdbm.TenantAccountStatusReady, sds[0].Status)
			}

			if test.org == org4 {
				taDAO := cdbm.NewTenantAccountDAO(dbSession)
				sdDAO := cdbm.NewStatusDetailDAO(dbSession)

				updatedTA, taErr := taDAO.GetByID(ctx, nil, ta4.ID, nil)
				require.NoError(t, taErr)
				assert.Equal(t, cdbm.TenantAccountStatusReady, updatedTA.Status)
				assert.True(t, updatedTA.Config.TargetedInstanceCreation)

				sds, sdErr := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{ta4.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
				require.NoError(t, sdErr)
				require.NotEmpty(t, sds, "service-account-promoted tenant account should have a status detail")
				assert.Equal(t, cdbm.TenantAccountStatusReady, sds[0].Status)
			}
		})
	}
}
