// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestCreateDomainHandler_Handle(t *testing.T) {
	tests := []struct {
		name string
		run  func(*testing.T)
	}{
		{name: "success", run: runCreateDomainHandlerSuccess},
		{name: "validation and authorization", run: runCreateDomainHandlerValidationAndAuthorization},
		{name: "reservation DB failure does not call Core", run: runCreateDomainHandlerCompensatesCoreAfterDatabaseFailure},
		{
			name: "Core rejection preserves Error reservation",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, nil)
				fixture.expectCore(t, corev1.Forge_CreateDomain_FullMethodName, nil, tp.NewNonRetryableApplicationError(
					"Domain rejected",
					swe.ErrTypeNICoFailedPrecondition,
					errors.New("Domain rejected"),
				))

				fixture.expectCore(t, corev1.Forge_FindDomain_FullMethodName, &corev1.DomainList{}, nil)
				fixture.expectCore(t, corev1.Forge_DeleteDomain_FullMethodName, nil, nil)
				recorder := fixture.request(t, NewCreateDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodPost, "/", "", model.APIDomainCreateRequest{
					Name:   "rejected.example.com",
					SiteID: fixture.site.ID.String(),
				})
				assert.Equal(t, http.StatusPreconditionFailed, recorder.Code, recorder.Body.String())
				fixture.requireDomainWithStatus(t, "rejected.example.com", cdbm.DomainStatusError)
			},
		},
		{
			name: "unexpected Core identity stays Pending",
			run: func(t *testing.T) {
				responses := []struct {
					name     string
					response *corev1.Domain
				}{
					{name: "missing", response: &corev1.Domain{}},
					{name: "malformed", response: &corev1.Domain{Id: &corev1.DomainId{Value: "not-a-uuid"}}},
					{name: "zero", response: &corev1.Domain{Id: &corev1.DomainId{Value: uuid.Nil.String()}}},
				}
				for _, response := range responses {
					t.Run(response.name, func(t *testing.T) {
						fixture := newDomainHandlerFixture(t, nil)
						fixture.expectCore(t, corev1.Forge_CreateDomain_FullMethodName, response.response, nil)
						recorder := fixture.request(t, NewCreateDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodPost, "/", "", model.APIDomainCreateRequest{
							Name:   "invalid-response.example.com",
							SiteID: fixture.site.ID.String(),
						})
						assert.Equal(t, http.StatusAccepted, recorder.Code, recorder.Body.String())
						fixture.requireDomainWithStatus(t, "invalid-response.example.com", cdbm.DomainStatusPending)
					})
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, tt.run)
	}
}

func runCreateDomainHandlerSuccess(t *testing.T) {
	fixture := newDomainHandlerFixture(t, nil)
	proxiedRequest := fixture.expectCore(t, corev1.Forge_CreateDomain_FullMethodName, &corev1.Domain{
		Name: "tenant.example.com",
	}, nil)

	recorder := fixture.request(t, NewCreateDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodPost, "/", "", model.APIDomainCreateRequest{
		Name:   "tenant.example.com",
		SiteID: fixture.site.ID.String(),
	})
	require.Equal(t, http.StatusCreated, recorder.Code, recorder.Body.String())

	var response model.APIDomain
	require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
	assert.Equal(t, "Ready", response.Status)
	assert.Equal(t, fixture.tenant.ID.String(), response.TenantID)
	assert.Equal(t, fixture.site.ID.String(), response.SiteID)
	assert.Equal(t, "tenant.example.com", response.Name)

	var coreRequest corev1.CreateDomainRequest
	require.NoError(t, protojson.Unmarshal(proxiedRequest.RequestJSON, &coreRequest))
	assert.Equal(t, "tenant.example.com", coreRequest.GetName())
	require.NotEmpty(t, coreRequest.GetReservedId().GetValue())
	assert.NotEqual(t, coreRequest.GetReservedId().GetValue(), response.ID)

	persisted, err := cdbm.NewDomainDAO(fixture.dbSession).GetByID(context.Background(), nil, uuid.MustParse(response.ID), nil)
	require.NoError(t, err)
	assert.Equal(t, &fixture.tenant.ID, persisted.TenantID)
	assert.Equal(t, &fixture.site.ID, persisted.SiteID)
	assert.Equal(t, coreRequest.GetReservedId().GetValue(), persisted.ControllerDomainID.String())
	assert.Equal(t, cdbm.DomainStatusReady, persisted.Status)
}

func runCreateDomainHandlerValidationAndAuthorization(t *testing.T) {
	fixture := newDomainHandlerFixture(t, nil)
	handler := NewCreateDomainHandler(fixture.dbSession, fixture.scp)

	recorder := fixture.request(t, handler.Handle, http.MethodPost, "/", "", "{")
	assert.Equal(t, http.StatusBadRequest, recorder.Code)
	assert.Contains(t, recorder.Body.String(), "Failed to parse request data, potentially invalid structure")

	recorder = fixture.request(t, handler.Handle, http.MethodPost, "/", "", model.APIDomainCreateRequest{SiteID: fixture.site.ID.String()})
	assert.Equal(t, http.StatusBadRequest, recorder.Code)
	assert.Contains(t, recorder.Body.String(), "Error validating Domain creation request data")

	otherSite := common.TestBuildSite(t, fixture.dbSession, fixture.provider, "Unauthorized Site", fixture.user)
	_, err := cdbm.NewSiteDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: otherSite.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)
	recorder = fixture.request(t, handler.Handle, http.MethodPost, "/", "", model.APIDomainCreateRequest{
		Name:   "tenant.example.com",
		SiteID: otherSite.ID.String(),
	})
	assert.Equal(t, http.StatusForbidden, recorder.Code)
	fixture.siteClient.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)

	fixture = newDomainHandlerFixture(t, nil)
	fixture.org = "different-org"
	recorder = fixture.request(t, NewCreateDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodPost, "/", "", model.APIDomainCreateRequest{
		Name:   "tenant.example.com",
		SiteID: fixture.site.ID.String(),
	})
	assert.Equal(t, http.StatusForbidden, recorder.Code, recorder.Body.String())
	fixture.siteClient.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
}

func runCreateDomainHandlerCompensatesCoreAfterDatabaseFailure(t *testing.T) {
	fixture := newDomainHandlerFixture(t, nil)
	_, err := fixture.dbSession.DB.ExecContext(context.Background(), `
		ALTER TABLE domain ADD CONSTRAINT domain_test_reject_hostname
		CHECK (hostname <> 'db-fail.example.com')
	`)
	require.NoError(t, err)

	recorder := fixture.request(t, NewCreateDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodPost, "/", "", model.APIDomainCreateRequest{
		Name:   "db-fail.example.com",
		SiteID: fixture.site.ID.String(),
	})
	assert.Equal(t, http.StatusInternalServerError, recorder.Code)

	domains, _, err := cdbm.NewDomainDAO(fixture.dbSession).GetAll(context.Background(), nil, cdbm.DomainFilterInput{}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	require.NoError(t, err)
	assert.Empty(t, domains)
}

func TestGetAllDomainHandler_Handle(t *testing.T) {
	tests := []struct {
		name string
		run  func(*testing.T)
	}{
		{
			name: "dual-role caller sees only current tenant rows",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, []string{authz.TenantAdminRole, authz.ProviderAdminRole})
				otherSite := common.TestBuildSite(t, fixture.dbSession, fixture.provider, "Other Site", fixture.user)
				common.TestBuildTenantSite(t, fixture.dbSession, fixture.tenant, otherSite, fixture.user)
				ownedSiteOne := fixture.createDomain(t, "one.example.com", &fixture.tenant.ID, &fixture.site.ID)
				ownedSiteTwo := fixture.createDomain(t, "two.example.com", &fixture.tenant.ID, &otherSite.ID)
				// A projection can survive Site-access revocation; the list must not leak it.
				unauthorizedSite := common.TestBuildSite(t, fixture.dbSession, fixture.provider, "Revoked Site", fixture.user)
				tenantSite := common.TestBuildTenantSite(t, fixture.dbSession, fixture.tenant, unauthorizedSite, fixture.user)
				fixture.createDomain(t, "revoked.example.com", &fixture.tenant.ID, &unauthorizedSite.ID)
				require.NoError(t, cdbm.NewTenantSiteDAO(fixture.dbSession).Delete(context.Background(), nil, tenantSite.ID))
				fixture.createDomain(t, "other.example.com", cutil.GetPtr(uuid.New()), &fixture.site.ID)
				fixture.createDomain(t, "legacy.example.com", nil, nil)

				recorder := fixture.request(t, NewGetAllDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", "", nil)
				require.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())
				var response []*model.APIDomain
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				require.Len(t, response, 2)
				assert.ElementsMatch(t, []string{ownedSiteOne.ID.String(), ownedSiteTwo.ID.String()}, []string{response[0].ID, response[1].ID})
				for _, domain := range response {
					assert.Equal(t, fixture.tenant.ID.String(), domain.TenantID)
				}

				var pageResponse pagination.PageResponse
				require.NoError(t, json.Unmarshal([]byte(recorder.Header().Get(pagination.ResponseHeaderName)), &pageResponse))
				assert.Equal(t, 1, pageResponse.PageNumber)
				assert.Equal(t, cdbp.DefaultLimit, pageResponse.PageSize)
				assert.Equal(t, 2, pageResponse.Total)
				assert.Nil(t, pageResponse.OrderBy)
			},
		},
		{
			name: "no accessible Sites returns an empty paginated list",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, nil)
				fixture.createDomain(t, "old.example.com", &fixture.tenant.ID, &fixture.site.ID)
				tenantSites, _, err := cdbm.NewTenantSiteDAO(fixture.dbSession).GetAll(context.Background(), nil,
					cdbm.TenantSiteFilterInput{TenantIDs: []uuid.UUID{fixture.tenant.ID}},
					cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
				require.NoError(t, err)
				for _, tenantSite := range tenantSites {
					require.NoError(t, cdbm.NewTenantSiteDAO(fixture.dbSession).Delete(context.Background(), nil, tenantSite.ID))
				}

				recorder := fixture.request(t, NewGetAllDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", "", nil)
				require.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())
				assert.JSONEq(t, `[]`, recorder.Body.String())
				var pageResponse pagination.PageResponse
				require.NoError(t, json.Unmarshal([]byte(recorder.Header().Get(pagination.ResponseHeaderName)), &pageResponse))
				assert.Zero(t, pageResponse.Total)
			},
		},
		{
			name: "tenant Site and pagination filters",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, nil)
				fixture.createDomain(t, "alpha.example.com", &fixture.tenant.ID, &fixture.site.ID)
				second := fixture.createDomain(t, "bravo.example.com", &fixture.tenant.ID, &fixture.site.ID)
				fixture.createDomain(t, "charlie.example.com", &fixture.tenant.ID, &fixture.site.ID)
				target := "/?tenantId=" + fixture.tenant.ID.String() + "&siteId=" + fixture.site.ID.String() + "&pageNumber=2&pageSize=1&orderBy=NAME_ASC"

				recorder := fixture.request(t, NewGetAllDomainHandler(fixture.dbSession).Handle, http.MethodGet, target, "", nil)
				require.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())
				var response []*model.APIDomain
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				require.Len(t, response, 1)
				assert.Equal(t, second.ID.String(), response[0].ID)

				var pageResponse pagination.PageResponse
				require.NoError(t, json.Unmarshal([]byte(recorder.Header().Get(pagination.ResponseHeaderName)), &pageResponse))
				assert.Equal(t, 2, pageResponse.PageNumber)
				assert.Equal(t, 1, pageResponse.PageSize)
				assert.Equal(t, 3, pageResponse.Total)
				require.NotNil(t, pageResponse.OrderBy)
				assert.Equal(t, "NAME_ASC", *pageResponse.OrderBy)
			},
		},
		{
			name: "invalid query inputs",
			run: func(t *testing.T) {
				targets := []string{
					"/?unknown=value",
					"/?tenantId=invalid",
					"/?siteId=invalid",
					"/?tenantId=" + uuid.NewString(),
					"/?pageNumber=-1",
					"/?pageSize=101",
					"/?orderBy=status_ASC",
				}
				for _, target := range targets {
					t.Run(target, func(t *testing.T) {
						fixture := newDomainHandlerFixture(t, nil)
						recorder := fixture.request(t, NewGetAllDomainHandler(fixture.dbSession).Handle, http.MethodGet, target, "", nil)
						assert.Equal(t, http.StatusBadRequest, recorder.Code, recorder.Body.String())
					})
				}
			},
		},
		{
			name: "unauthorized Site filter",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, nil)
				otherSite := common.TestBuildSite(t, fixture.dbSession, fixture.provider, "Unauthorized Site", fixture.user)
				recorder := fixture.request(t, NewGetAllDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/?siteId="+otherSite.ID.String(), "", nil)
				assert.Equal(t, http.StatusForbidden, recorder.Code, recorder.Body.String())
			},
		},
		{
			name: "missing user and org membership",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, nil)
				fixture.user = nil
				recorder := fixture.request(t, NewGetAllDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", "", nil)
				assert.Equal(t, http.StatusInternalServerError, recorder.Code, recorder.Body.String())

				fixture = newDomainHandlerFixture(t, nil)
				fixture.org = "different-org"
				recorder = fixture.request(t, NewGetAllDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", "", nil)
				assert.Equal(t, http.StatusForbidden, recorder.Code, recorder.Body.String())
			},
		},
		{
			name: "wrong role",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, []string{authz.ProviderAdminRole})
				recorder := fixture.request(t, NewGetAllDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", "", nil)
				assert.Equal(t, http.StatusForbidden, recorder.Code, recorder.Body.String())
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, tt.run)
	}
}

func TestGetDomainHandler_Handle(t *testing.T) {
	tests := []struct {
		name string
		run  func(*testing.T)
	}{
		{
			name: "success uses REST local identity",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, nil)
				domain := fixture.createDomain(t, "owned.example.com", &fixture.tenant.ID, &fixture.site.ID)
				recorder := fixture.request(t, NewGetDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", domain.ID.String(), nil)
				require.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())
				var response model.APIDomain
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				assert.Equal(t, domain.ID.String(), response.ID)
				assert.Equal(t, fixture.tenant.ID.String(), response.TenantID)
			},
		},
		{
			name: "Site access revocation hides a known Domain ID",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, nil)
				domain := fixture.createDomain(t, "revoked.example.com", &fixture.tenant.ID, &fixture.site.ID)
				handler := NewGetDomainHandler(fixture.dbSession)
				recorder := fixture.request(t, handler.Handle, http.MethodGet, "/", domain.ID.String(), nil)
				require.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())

				tenantSite, err := cdbm.NewTenantSiteDAO(fixture.dbSession).GetByTenantIDAndSiteID(context.Background(), nil, fixture.tenant.ID, fixture.site.ID, nil)
				require.NoError(t, err)
				require.NoError(t, cdbm.NewTenantSiteDAO(fixture.dbSession).Delete(context.Background(), nil, tenantSite.ID))

				recorder = fixture.request(t, handler.Handle, http.MethodGet, "/", domain.ID.String(), nil)
				assert.Equal(t, http.StatusForbidden, recorder.Code, recorder.Body.String())
			},
		},
		{
			name: "malformed and zero IDs",
			run: func(t *testing.T) {
				for _, id := range []string{"invalid", uuid.Nil.String()} {
					t.Run(id, func(t *testing.T) {
						fixture := newDomainHandlerFixture(t, nil)
						recorder := fixture.request(t, NewGetDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", id, nil)
						assert.Equal(t, http.StatusBadRequest, recorder.Code, recorder.Body.String())
					})
				}
			},
		},
		{
			name: "unknown cross-tenant legacy and controller IDs are hidden",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, nil)
				controllerID := uuid.New()
				owned := fixture.createDomainWithControllerID(t, "owned.example.com", &fixture.tenant.ID, &fixture.site.ID, controllerID)
				crossTenant := fixture.createDomain(t, "other.example.com", cutil.GetPtr(uuid.New()), &fixture.site.ID)
				legacy := fixture.createDomain(t, "legacy.example.com", nil, nil)
				for _, id := range []uuid.UUID{uuid.New(), controllerID, crossTenant.ID, legacy.ID} {
					recorder := fixture.request(t, NewGetDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", id.String(), nil)
					assert.Equal(t, http.StatusNotFound, recorder.Code, recorder.Body.String())
				}
				recorder := fixture.request(t, NewGetDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", owned.ID.String(), nil)
				assert.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())
			},
		},
		{
			name: "missing user org membership and wrong role",
			run: func(t *testing.T) {
				fixture := newDomainHandlerFixture(t, nil)
				fixture.user = nil
				recorder := fixture.request(t, NewGetDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", uuid.NewString(), nil)
				assert.Equal(t, http.StatusInternalServerError, recorder.Code, recorder.Body.String())

				fixture = newDomainHandlerFixture(t, nil)
				fixture.org = "different-org"
				recorder = fixture.request(t, NewGetDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", uuid.NewString(), nil)
				assert.Equal(t, http.StatusForbidden, recorder.Code, recorder.Body.String())

				fixture = newDomainHandlerFixture(t, []string{authz.ProviderAdminRole})
				recorder = fixture.request(t, NewGetDomainHandler(fixture.dbSession).Handle, http.MethodGet, "/", uuid.NewString(), nil)
				assert.Equal(t, http.StatusForbidden, recorder.Code, recorder.Body.String())
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, tt.run)
	}
}

func TestDeleteDomainHandler_Handle(t *testing.T) {
	tests := []struct {
		name            string
		coreError       error
		expectedStatus  int
		expectedDeleted bool
	}{
		{
			name:            "success",
			expectedStatus:  http.StatusNoContent,
			expectedDeleted: true,
		},
		{
			name: "Core failed precondition preserves Deleting reservation",
			coreError: tp.NewNonRetryableApplicationError(
				"Domain is in use",
				swe.ErrTypeNICoFailedPrecondition,
				errors.New("Domain is in use"),
			),
			expectedStatus: http.StatusPreconditionFailed,
		},
		{
			name: "Core not found retains Deleting reservation",
			coreError: tp.NewNonRetryableApplicationError(
				"Domain not found",
				swe.ErrTypeNICoObjectNotFound,
				errors.New("Domain not found"),
			),
			expectedStatus: http.StatusNotFound,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newDomainHandlerFixture(t, nil)
			controllerDomainID := uuid.New()
			domain := fixture.createDomainWithControllerID(t, "delete.example.com", &fixture.tenant.ID, &fixture.site.ID, controllerDomainID)
			proxiedRequest := fixture.expectCore(t, corev1.Forge_DeleteDomain_FullMethodName, nil, tt.coreError)

			recorder := fixture.request(t, NewDeleteDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodDelete, "/", domain.ID.String(), nil)
			assert.Equal(t, tt.expectedStatus, recorder.Code, recorder.Body.String())

			var coreRequest corev1.DomainDeletionRequest
			require.NoError(t, protojson.Unmarshal(proxiedRequest.RequestJSON, &coreRequest))
			assert.Equal(t, controllerDomainID.String(), coreRequest.GetId().GetValue())
			assert.NotEqual(t, domain.ID.String(), coreRequest.GetId().GetValue())
			assert.True(t, coreRequest.GetCancelReservedId())

			persisted, err := cdbm.NewDomainDAO(fixture.dbSession).GetByID(context.Background(), nil, domain.ID, nil)
			if tt.expectedDeleted {
				assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
				assert.Nil(t, persisted)
			} else {
				require.NoError(t, err)
				assert.Equal(t, domain.ID, persisted.ID)
				assert.Equal(t, cdbm.DomainStatusDeleting, persisted.Status)
			}
		})
	}

	t.Run("local Subnet reference", runDeleteDomainHandlerRejectsLocalSubnetReference)

	t.Run("projection transaction failure rolls back", func(t *testing.T) {
		fixture := newDomainHandlerFixture(t, nil)
		controllerDomainID := uuid.New()
		domain := fixture.createDomainWithControllerID(t, "delete-failure.example.com", &fixture.tenant.ID, &fixture.site.ID, controllerDomainID)
		_, err := fixture.dbSession.DB.ExecContext(context.Background(), `
			ALTER TABLE domain ADD CONSTRAINT domain_test_reject_delete
			CHECK (deleted IS NULL)
		`)
		require.NoError(t, err)
		proxiedRequest := fixture.expectCore(t, corev1.Forge_DeleteDomain_FullMethodName, nil, nil)

		recorder := fixture.request(t, NewDeleteDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodDelete, "/", domain.ID.String(), nil)
		assert.Equal(t, http.StatusInternalServerError, recorder.Code, recorder.Body.String())

		var coreRequest corev1.DomainDeletionRequest
		require.NoError(t, protojson.Unmarshal(proxiedRequest.RequestJSON, &coreRequest))
		assert.Equal(t, controllerDomainID.String(), coreRequest.GetId().GetValue())
		persisted, err := cdbm.NewDomainDAO(fixture.dbSession).GetByID(context.Background(), nil, domain.ID, nil)
		require.NoError(t, err)
		assert.Equal(t, domain.ID, persisted.ID)
		assert.Equal(t, cdbm.DomainStatusDeleting, persisted.Status)
	})

	t.Run("invalid and hidden identities do not reach Core", func(t *testing.T) {
		fixture := newDomainHandlerFixture(t, nil)
		controllerDomainID := uuid.New()
		fixture.createDomainWithControllerID(t, "owned.example.com", &fixture.tenant.ID, &fixture.site.ID, controllerDomainID)
		crossTenant := fixture.createDomain(t, "other.example.com", cutil.GetPtr(uuid.New()), &fixture.site.ID)
		legacy := fixture.createDomain(t, "legacy.example.com", nil, nil)
		cases := []struct {
			name   string
			id     string
			status int
		}{
			{name: "malformed", id: "invalid", status: http.StatusBadRequest},
			{name: "zero", id: uuid.Nil.String(), status: http.StatusBadRequest},
			{name: "unknown", id: uuid.NewString(), status: http.StatusNotFound},
			{name: "controller identity", id: controllerDomainID.String(), status: http.StatusNotFound},
			{name: "cross-tenant", id: crossTenant.ID.String(), status: http.StatusNotFound},
			{name: "legacy", id: legacy.ID.String(), status: http.StatusNotFound},
		}
		for _, tc := range cases {
			t.Run(tc.name, func(t *testing.T) {
				recorder := fixture.request(t, NewDeleteDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodDelete, "/", tc.id, nil)
				assert.Equal(t, tc.status, recorder.Code, recorder.Body.String())
			})
		}
		fixture.siteClient.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
	})

	t.Run("Site membership is enforced", func(t *testing.T) {
		fixture := newDomainHandlerFixture(t, nil)
		unauthorizedSite := common.TestBuildSite(t, fixture.dbSession, fixture.provider, "Unauthorized Site", fixture.user)
		domain := fixture.createDomain(t, "unauthorized.example.com", &fixture.tenant.ID, &unauthorizedSite.ID)
		recorder := fixture.request(t, NewDeleteDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodDelete, "/", domain.ID.String(), nil)
		assert.Equal(t, http.StatusForbidden, recorder.Code, recorder.Body.String())
		fixture.siteClient.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
	})

	t.Run("missing user org membership and wrong role", func(t *testing.T) {
		fixture := newDomainHandlerFixture(t, nil)
		fixture.user = nil
		recorder := fixture.request(t, NewDeleteDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodDelete, "/", uuid.NewString(), nil)
		assert.Equal(t, http.StatusInternalServerError, recorder.Code, recorder.Body.String())

		fixture = newDomainHandlerFixture(t, nil)
		fixture.org = "different-org"
		recorder = fixture.request(t, NewDeleteDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodDelete, "/", uuid.NewString(), nil)
		assert.Equal(t, http.StatusForbidden, recorder.Code, recorder.Body.String())

		fixture = newDomainHandlerFixture(t, []string{authz.ProviderAdminRole})
		recorder = fixture.request(t, NewDeleteDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodDelete, "/", uuid.NewString(), nil)
		assert.Equal(t, http.StatusForbidden, recorder.Code, recorder.Body.String())
	})
}

func runDeleteDomainHandlerRejectsLocalSubnetReference(t *testing.T) {
	fixture := newDomainHandlerFixture(t, nil)
	domain := fixture.createDomain(t, "in-use.example.com", &fixture.tenant.ID, &fixture.site.ID)
	vpc := common.TestBuildVPC(t, fixture.dbSession, "test-vpc", fixture.provider, fixture.tenant, fixture.site, cutil.GetPtr(uuid.New()), nil, nil, cdbm.VpcStatusReady, fixture.user)
	subnet := common.TestBuildSubnet(t, fixture.dbSession, "test-subnet", fixture.tenant, vpc, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, fixture.user)
	_, err := cdbm.NewSubnetDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.SubnetUpdateInput{
		SubnetId: subnet.ID,
		DomainID: &domain.ID,
	})
	require.NoError(t, err)

	recorder := fixture.request(t, NewDeleteDomainHandler(fixture.dbSession, fixture.scp).Handle, http.MethodDelete, "/", domain.ID.String(), nil)
	assert.Equal(t, http.StatusPreconditionFailed, recorder.Code, recorder.Body.String())
	fixture.siteClient.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)

	persisted, err := cdbm.NewDomainDAO(fixture.dbSession).GetByID(context.Background(), nil, domain.ID, nil)
	require.NoError(t, err)
	assert.Equal(t, domain.ID, persisted.ID)
}

type domainHandlerFixture struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	siteClient *tmocks.Client
	org        string
	user       *cdbm.User
	provider   *cdbm.InfrastructureProvider
	tenant     *cdbm.Tenant
	site       *cdbm.Site
}

func newDomainHandlerFixture(t *testing.T, roles []string) *domainHandlerFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)
	if roles == nil {
		roles = []string{authz.TenantAdminRole}
	}

	org := "domain-test-tenant-org"
	user := common.TestBuildUser(t, dbSession, uuid.NewString(), org, roles)
	providerUser := common.TestBuildUser(t, dbSession, uuid.NewString(), "domain-test-provider-org", []string{authz.ProviderAdminRole})
	provider := common.TestBuildInfrastructureProvider(t, dbSession, "Domain Test Provider", "domain-test-provider-org", providerUser)
	site := common.TestBuildSite(t, dbSession, provider, "Domain Test Site", providerUser)
	_, err := cdbm.NewSiteDAO(dbSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)
	site.Status = cdbm.SiteStatusRegistered
	tenant := common.TestBuildTenant(t, dbSession, "Domain Test Tenant", org, user)
	common.TestBuildTenantSite(t, dbSession, tenant, site, user)

	siteClient := &tmocks.Client{}
	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = siteClient

	return &domainHandlerFixture{
		dbSession: dbSession, scp: scp, siteClient: siteClient, org: org, user: user,
		provider: provider, tenant: tenant, site: site,
	}
}

func (f *domainHandlerFixture) expectCore(t *testing.T, fullMethod string, response proto.Message, resultErr error) *grpcproxy.Request {
	t.Helper()
	return f.expectCoreWithCallback(t, fullMethod, response, resultErr, nil, nil)
}

func (f *domainHandlerFixture) expectCoreWithCallback(
	t *testing.T,
	fullMethod string,
	response proto.Message,
	resultErr error,
	afterGet func(),
	contextCheck func(context.Context) bool,
) *grpcproxy.Request {
	t.Helper()

	proxiedRequest := &grpcproxy.Request{}
	workflowRun := &tmocks.WorkflowRun{}
	workflowRun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		if response != nil {
			// A successful reserved create must return the exact caller-reserved ID,
			// not a second test-generated ID disconnected from the durable row.
			if d, ok := response.(*corev1.Domain); ok && d.GetId() == nil && d.GetName() != "" && fullMethod == corev1.Forge_CreateDomain_FullMethodName {
				var create corev1.CreateDomainRequest
				require.NoError(t, protojson.Unmarshal(proxiedRequest.RequestJSON, &create))
				require.NotEmpty(t, create.GetReservedId().GetValue())
				response = proto.Clone(d)
				response.(*corev1.Domain).Id = create.GetReservedId()
			}
			responseJSON, err := protojson.Marshal(response)
			require.NoError(t, err)
			args.Get(1).(*grpcproxy.Response).ResponseJSON = responseJSON
		}
		if afterGet != nil {
			afterGet()
		}
	}).Return(resultErr).Once()
	f.siteClient.On("ExecuteWorkflow", mock.MatchedBy(func(ctx context.Context) bool {
		return contextCheck == nil || contextCheck(ctx)
	}), mock.Anything, grpcproxy.Core.WorkflowName, mock.MatchedBy(func(request grpcproxy.Request) bool {
		if request.FullMethod != fullMethod {
			return false
		}
		*proxiedRequest = request
		return true
	})).Return(workflowRun, nil).Once()
	t.Cleanup(func() {
		workflowRun.AssertExpectations(t)
		f.siteClient.AssertExpectations(t)
	})
	return proxiedRequest
}

func (f *domainHandlerFixture) requireDomainWithStatus(t *testing.T, name, status string) {
	t.Helper()
	domains, _, err := cdbm.NewDomainDAO(f.dbSession).GetAll(context.Background(), nil,
		cdbm.DomainFilterInput{TenantIDs: []uuid.UUID{f.tenant.ID}},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	require.NoError(t, err)
	require.Len(t, domains, 1)
	assert.Equal(t, name, domains[0].Hostname)
	assert.Equal(t, status, domains[0].Status)
	assert.NotNil(t, domains[0].ControllerDomainID)
}

func (f *domainHandlerFixture) requireNoDomains(t *testing.T) {
	t.Helper()
	domains, _, err := cdbm.NewDomainDAO(f.dbSession).GetAll(context.Background(), nil, cdbm.DomainFilterInput{}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	require.NoError(t, err)
	assert.Empty(t, domains)
}

func (f *domainHandlerFixture) createDomain(t *testing.T, name string, tenantID, siteID *uuid.UUID) *cdbm.Domain {
	t.Helper()
	return f.createDomainWithControllerID(t, name, tenantID, siteID, uuid.New())
}

func (f *domainHandlerFixture) createDomainWithControllerID(t *testing.T, name string, tenantID, siteID *uuid.UUID, controllerDomainID uuid.UUID) *cdbm.Domain {
	t.Helper()
	domain, err := cdbm.NewDomainDAO(f.dbSession).Create(context.Background(), nil, cdbm.DomainCreateInput{
		Hostname:           name,
		Org:                f.org,
		TenantID:           tenantID,
		SiteID:             siteID,
		ControllerDomainID: &controllerDomainID,
		Status:             cdbm.DomainStatusReady,
		CreatedBy:          f.user.ID,
	})
	require.NoError(t, err)
	return domain
}

func (f *domainHandlerFixture) request(
	t *testing.T,
	handler func(echo.Context) error,
	method string,
	target string,
	id string,
	body any,
) *httptest.ResponseRecorder {
	t.Helper()
	return f.requestWithContext(t, context.Background(), handler, method, target, id, body)
}

func (f *domainHandlerFixture) requestWithContext(
	t *testing.T,
	ctx context.Context,
	handler func(echo.Context) error,
	method string,
	target string,
	id string,
	body any,
) *httptest.ResponseRecorder {
	t.Helper()

	requestBody := ""
	if rawBody, ok := body.(string); ok {
		requestBody = rawBody
	} else if body != nil {
		encodedBody, err := json.Marshal(body)
		require.NoError(t, err)
		requestBody = string(encodedBody)
	}

	request := httptest.NewRequest(method, target, strings.NewReader(requestBody)).WithContext(ctx)
	request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	recorder := httptest.NewRecorder()
	e := echo.New()
	echoContext := e.NewContext(request, recorder)
	echoContext.SetParamNames("orgName", "domainId")
	echoContext.SetParamValues(f.org, id)
	echoContext.Set("user", f.user)
	require.NoError(t, handler(echoContext))
	return recorder
}
