// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

func TestGetAllInterfaceSummaryHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	providerOrg := "vpc-attachment-provider"
	ownerOrg := "vpc-attachment-owner"
	consumerOrg := "vpc-attachment-consumer"
	providerUser := testInstanceBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{authz.ProviderAdminRole})
	ownerUser := testInstanceBuildUser(t, dbSession, uuid.NewString(), ownerOrg, []string{authz.TenantAdminRole})
	consumerUser := testInstanceBuildUser(t, dbSession, uuid.NewString(), consumerOrg, []string{authz.TenantAdminRole})
	provider := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "Attachment Provider", providerOrg, providerUser)
	site := testInstanceBuildSite(t, dbSession, provider, "Attachment Site", cdbm.SiteStatusRegistered, false, providerUser)
	ownerTenant := testInstanceBuildTenant(t, dbSession, "Attachment Owner", ownerOrg, ownerUser)
	consumerTenant := testInstanceBuildTenant(t, dbSession, "Attachment Consumer", consumerOrg, consumerUser)

	ownerControllerVpcID := uuid.New()
	consumerControllerVpcID := uuid.New()
	ownerVpc := testInstanceBuildVPC(
		t, dbSession, "Attachment Owner VPC", provider, ownerTenant, site,
		&ownerControllerVpcID, map[string]string{}, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, ownerUser,
	)
	emptyVpc := testInstanceBuildVPC(
		t, dbSession, "Empty Owner VPC", provider, ownerTenant, site,
		cutil.GetPtr(uuid.New()), map[string]string{}, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, ownerUser,
	)
	consumerVpc := testInstanceBuildVPC(
		t, dbSession, "Consumer VPC", provider, consumerTenant, site,
		&consumerControllerVpcID, map[string]string{}, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, consumerUser,
	)

	ownerInstance := testInstanceBuildInstance(
		t, dbSession, "Owner Instance", ownerTenant.ID, provider.ID, site.ID,
		nil, ownerVpc.ID, nil, nil, nil, cdbm.InstanceStatusReady,
	)
	consumerInstance := testInstanceBuildInstance(
		t, dbSession, "Cross-Tenant Instance", consumerTenant.ID, provider.ID, site.ID,
		nil, consumerVpc.ID, nil, nil, nil, cdbm.InstanceStatusReady,
	)
	vpcPrefix := testInstanceBuildVPCPrefix(
		t, dbSession, "Attachment Prefix", ownerTenant, ownerVpc, nil,
		"192.0.2.0/24", 24, cdbm.VpcPrefixStatusReady, ownerUser,
	)

	ifcDAO := cdbm.NewInterfaceDAO(dbSession)
	ownerDirect, err := ifcDAO.Create(ctx, nil, cdbm.InterfaceCreateInput{
		InstanceID: ownerInstance.ID,
		VpcID:      &ownerVpc.ID,
		IsPhysical: true,
		Status:     cdbm.InterfaceStatusReady,
		CreatedBy:  ownerUser.ID,
	})
	require.NoError(t, err)
	device := "eth1"
	deviceInstance := 1
	crossTenantDirect, err := ifcDAO.Create(ctx, nil, cdbm.InterfaceCreateInput{
		InstanceID:     consumerInstance.ID,
		VpcID:          &ownerVpc.ID,
		Device:         &device,
		DeviceInstance: &deviceInstance,
		Status:         cdbm.InterfaceStatusPending,
		CreatedBy:      consumerUser.ID,
	})
	require.NoError(t, err)
	virtualFunctionID := 2
	crossTenantPrefix, err := ifcDAO.Create(ctx, nil, cdbm.InterfaceCreateInput{
		InstanceID:        consumerInstance.ID,
		VpcPrefixID:       &vpcPrefix.ID,
		Device:            &device,
		DeviceInstance:    &deviceInstance,
		VirtualFunctionID: &virtualFunctionID,
		Status:            cdbm.InterfaceStatusProvisioning,
		CreatedBy:         consumerUser.ID,
	})
	require.NoError(t, err)
	consumerOwn, err := ifcDAO.Create(ctx, nil, cdbm.InterfaceCreateInput{
		InstanceID: consumerInstance.ID,
		VpcID:      &consumerVpc.ID,
		Status:     cdbm.InterfaceStatusReady,
		CreatedBy:  consumerUser.ID,
	})
	require.NoError(t, err)

	handler := NewGetAllInterfaceSummaryHandler(dbSession)
	checkSummaries := func(wantIDs []string, pageSize int, checkFields bool) func(*httptest.ResponseRecorder) {
		return func(rec *httptest.ResponseRecorder) {
			var summaries []model.APIInterfaceSummary
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &summaries))
			gotIDs := make([]string, 0, len(summaries))
			for _, summary := range summaries {
				gotIDs = append(gotIDs, summary.ID)
			}
			assert.ElementsMatch(t, wantIDs, gotIDs)

			if checkFields {
				var raw []map[string]json.RawMessage
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &raw))
				allowedFields := map[string]struct{}{
					"id": {}, "vpcId": {}, "vpcPrefixId": {}, "instanceId": {}, "instanceTenantId": {},
					"device": {}, "deviceInstance": {}, "isPhysical": {}, "virtualFunctionId": {},
					"status": {}, "created": {}, "updated": {},
				}
				for _, summary := range raw {
					fields := make(map[string]struct{}, len(summary))
					for field := range summary {
						fields[field] = struct{}{}
					}
					assert.Equal(t, allowedFields, fields)
				}
			}

			var pageResponse pagination.PageResponse
			require.NoError(t, json.Unmarshal([]byte(rec.Header().Get(pagination.ResponseHeaderName)), &pageResponse))
			assert.Equal(t, len(wantIDs), pageResponse.Total)
			assert.Equal(t, pageSize, pageResponse.PageSize)
		}
	}

	tests := []struct {
		name       string
		org        string
		user       *cdbm.User
		query      url.Values
		statusCode int
		check      func(*httptest.ResponseRecorder)
	}{
		{
			name:       "default list includes own Interfaces and cross-Tenant Interfaces on owned VPCs",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"pageSize": {"100"}, "orderBy": {"CREATED_ASC"}},
			statusCode: http.StatusOK,
			check: checkSummaries(
				[]string{ownerDirect.ID.String(), crossTenantDirect.ID.String(), crossTenantPrefix.ID.String()},
				100,
				true,
			),
		},
		{
			name:       "cross-Tenant filter returns only other Tenants on owned VPCs",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"crossTenant": {"true"}, "vpcId": {ownerVpc.ID.String()}},
			statusCode: http.StatusOK,
			check: checkSummaries(
				[]string{crossTenantDirect.ID.String(), crossTenantPrefix.ID.String()},
				cdbp.DefaultLimit,
				false,
			),
		},
		{
			name:       "same-Tenant filter returns only caller-owned Instances",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"crossTenant": {"false"}},
			statusCode: http.StatusOK,
			check:      checkSummaries([]string{ownerDirect.ID.String()}, cdbp.DefaultLimit, false),
		},
		{
			name:       "Instance Tenant filter is constrained by visibility",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"instanceTenantId": {consumerTenant.ID.String()}},
			statusCode: http.StatusOK,
			check: checkSummaries(
				[]string{crossTenantDirect.ID.String(), crossTenantPrefix.ID.String()},
				cdbp.DefaultLimit,
				false,
			),
		},
		{
			name:       "caller sees all own Instance Interfaces across VPC ownership",
			org:        consumerOrg,
			user:       consumerUser,
			statusCode: http.StatusOK,
			check: checkSummaries(
				[]string{crossTenantDirect.ID.String(), crossTenantPrefix.ID.String(), consumerOwn.ID.String()},
				cdbp.DefaultLimit,
				false,
			),
		},
		{
			name:       "inaccessible VPC filter returns an empty array",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"vpcId": {consumerVpc.ID.String()}},
			statusCode: http.StatusOK,
			check: func(rec *httptest.ResponseRecorder) {
				assert.Equal(t, "[]", strings.TrimSpace(rec.Body.String()))
			},
		},
		{
			name:       "owned VPC without Interfaces returns an empty array",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"vpcId": {emptyVpc.ID.String()}},
			statusCode: http.StatusOK,
			check: func(rec *httptest.ResponseRecorder) {
				assert.Equal(t, "[]", strings.TrimSpace(rec.Body.String()))
			},
		},
		{
			name: "conflicting cross-Tenant and Instance Tenant filters return an empty array",
			org:  ownerOrg,
			user: ownerUser,
			query: url.Values{
				"crossTenant":      {"true"},
				"instanceTenantId": {ownerTenant.ID.String()},
			},
			statusCode: http.StatusOK,
			check: func(rec *httptest.ResponseRecorder) {
				assert.Equal(t, "[]", strings.TrimSpace(rec.Body.String()))
			},
		},
		{
			name:       "invalid VPC ID is rejected",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"vpcId": {"not-a-uuid"}},
			statusCode: http.StatusBadRequest,
		},
		{
			name:       "invalid Instance Tenant ID is rejected",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"instanceTenantId": {"not-a-uuid"}},
			statusCode: http.StatusBadRequest,
		},
		{
			name:       "invalid cross-Tenant boolean is rejected",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"crossTenant": {"not-a-boolean"}},
			statusCode: http.StatusBadRequest,
		},
		{
			name:       "unknown query parameter is rejected",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"unknown": {"value"}},
			statusCode: http.StatusBadRequest,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			target := "/"
			if len(tc.query) > 0 {
				target += "?" + tc.query.Encode()
			}
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, target, nil)
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.org)
			ec.Set("user", tc.user)
			ec.SetRequest(ec.Request().WithContext(ctx))

			require.NoError(t, handler.Handle(ec))
			assert.Equal(t, tc.statusCode, rec.Code)
			if tc.check != nil {
				tc.check(rec)
			}
		})
	}
}
