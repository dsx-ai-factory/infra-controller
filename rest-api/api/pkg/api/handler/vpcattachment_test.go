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
)

func TestGetAllVPCAttachmentHandler_Handle(t *testing.T) {
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

	handler := NewGetAllVPCAttachmentHandler(dbSession)
	tests := []struct {
		name       string
		org        string
		user       *cdbm.User
		vpcID      string
		query      url.Values
		statusCode int
		check      func(*httptest.ResponseRecorder)
	}{
		{
			name:       "owner sees same-Tenant and cross-Tenant direct and prefix attachments",
			org:        ownerOrg,
			user:       ownerUser,
			vpcID:      ownerVpc.ID.String(),
			query:      url.Values{"pageSize": {"100"}, "orderBy": {"CREATED_ASC"}},
			statusCode: http.StatusOK,
			check: func(rec *httptest.ResponseRecorder) {
				var attachments []model.APIVpcAttachment
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &attachments))
				require.Len(t, attachments, 3)
				assert.ElementsMatch(t,
					[]string{ownerDirect.ID.String(), crossTenantDirect.ID.String(), crossTenantPrefix.ID.String()},
					[]string{attachments[0].ID, attachments[1].ID, attachments[2].ID},
				)
				assert.ElementsMatch(t,
					[]string{ownerTenant.ID.String(), consumerTenant.ID.String(), consumerTenant.ID.String()},
					[]string{attachments[0].InstanceTenantID, attachments[1].InstanceTenantID, attachments[2].InstanceTenantID},
				)

				var raw []map[string]json.RawMessage
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &raw))
				allowedFields := map[string]struct{}{
					"id": {}, "vpcId": {}, "vpcPrefixId": {}, "instanceId": {}, "instanceTenantId": {},
					"device": {}, "deviceInstance": {}, "isPhysical": {}, "virtualFunctionId": {},
					"status": {}, "created": {}, "updated": {},
				}
				for _, attachment := range raw {
					assert.Equal(t, allowedFields, func() map[string]struct{} {
						fields := make(map[string]struct{}, len(attachment))
						for field := range attachment {
							fields[field] = struct{}{}
						}
						return fields
					}())
				}

				var pageResponse pagination.PageResponse
				require.NoError(t, json.Unmarshal([]byte(rec.Header().Get(pagination.ResponseHeaderName)), &pageResponse))
				assert.Equal(t, 3, pageResponse.Total)
				assert.Equal(t, 100, pageResponse.PageSize)
			},
		},
		{
			name:       "different Tenant cannot list attachments",
			org:        consumerOrg,
			user:       consumerUser,
			vpcID:      ownerVpc.ID.String(),
			statusCode: http.StatusForbidden,
		},
		{
			name:       "empty attachment list is an array",
			org:        ownerOrg,
			user:       ownerUser,
			vpcID:      emptyVpc.ID.String(),
			statusCode: http.StatusOK,
			check: func(rec *httptest.ResponseRecorder) {
				assert.Equal(t, "[]", strings.TrimSpace(rec.Body.String()))
			},
		},
		{
			name:       "invalid VPC ID is rejected",
			org:        ownerOrg,
			user:       ownerUser,
			vpcID:      "not-a-uuid",
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
			ec.SetParamNames("orgName", "vpcId")
			ec.SetParamValues(tc.org, tc.vpcID)
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
