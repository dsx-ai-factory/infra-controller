// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"reflect"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

func TestGetAllInstanceInterfaceHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		reqInstance   *cdbm.Instance
		reqInstanceID string
		reqOrg        string
		reqUser       *cdbm.User
		reqMachine    *cdbm.Machine
		respCode      int
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrgRoles1 := []string{authz.TenantAdminRole}

	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles2 := []string{authz.TenantAdminRole}

	ipu := testInstanceBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg1, tnOrgRoles1)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles2)

	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)

	mc1 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip, tn1, st1, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpc2)

	subnets := []*cdbm.Subnet{}
	for i := 11; i <= 35; i++ {
		subnet1 := testInstanceBuildSubnet(t, dbSession, fmt.Sprintf("test-subnet-%d", i), tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
		assert.NotNil(t, subnet1)
		subnets = append(subnets, subnet1)
	}

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnets[0].ID, mc1.ID)
	assert.NotNil(t, mci1)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)

	for i := 0; i < 25; i++ {
		instsub1 := testInstanceBuildInstanceInterface(t, dbSession, inst1.ID, &subnets[i].ID, nil, nil, cdbm.InterfaceStatusProvisioning)
		assert.NotNil(t, instsub1)
	}

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                      string
		fields                    fields
		args                      args
		wantErr                   bool
		queryStatus               *string
		queryIncludeRelations1    *string
		queryIncludeRelations2    *string
		pageNumber                *int
		pageSize                  *int
		orderBy                   *string
		expectSubnet              bool
		expectedFirstSubnetID     string
		expectedFirstSubnetName   *string
		expectedFirstInstanceName *string
		expectedCount             int
		expectedTotal             int
		verifyChildSpanner        bool
		query                     url.Values
	}{
		{
			name: "test Interface getall subnet API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			wantErr:               false,
			orderBy:               cutil.GetPtr("CREATED_ASC"),
			expectedCount:         20,
			expectedTotal:         25,
			expectSubnet:          true,
			expectedFirstSubnetID: subnets[0].ID.String(),
			verifyChildSpanner:    true,
		},
		{
			name: "test Interface getall success with paging",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			wantErr:               false,
			pageNumber:            cutil.GetPtr(1),
			pageSize:              cutil.GetPtr(10),
			orderBy:               cutil.GetPtr("CREATED_ASC"),
			expectedCount:         10,
			expectedTotal:         25,
			expectSubnet:          true,
			expectedFirstSubnetID: subnets[0].ID.String(),
		},
		{
			name: "test Interface getall success with paging on page 2",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			wantErr:               false,
			pageNumber:            cutil.GetPtr(2),
			pageSize:              cutil.GetPtr(10),
			orderBy:               cutil.GetPtr("CREATED_ASC"),
			expectedCount:         10,
			expectedTotal:         25,
			expectSubnet:          true,
			expectedFirstSubnetID: subnets[10].ID.String(),
		},
		{
			name: "test Interface getall error with paging bad orderby",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusBadRequest,
			},
			wantErr:      false,
			pageNumber:   cutil.GetPtr(2),
			pageSize:     cutil.GetPtr(10),
			orderBy:      cutil.GetPtr("TEST_ASC"),
			expectSubnet: true,
		},
		{
			name: "test Instance getall subnet API failure, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqOrg:      ipOrg,
				reqUser:     ipu,
				respCode:    http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test Instance getall subnet API failure, invalid Instance ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance getall subnet API failure, Instance ID in request not found",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: uuid.New().String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusNotFound,
			},
			wantErr: false,
		},
		{
			name: "test Instance getall subnet API failure, Instance not belong to current tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg2,
				reqUser:       tnu2,
				respCode:      http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test Instance getall subnet API endpoint success include relation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			queryIncludeRelations1:    cutil.GetPtr(cdbm.SubnetRelationName),
			queryIncludeRelations2:    cutil.GetPtr(cdbm.InstanceRelationName),
			expectedCount:             20,
			expectedTotal:             25,
			orderBy:                   cutil.GetPtr("CREATED_ASC"),
			expectSubnet:              true,
			expectedFirstSubnetID:     subnets[0].ID.String(),
			expectedFirstSubnetName:   cutil.GetPtr(subnets[0].Name),
			expectedFirstInstanceName: cutil.GetPtr(inst1.Name),
			wantErr:                   false,
		},
		{
			name: "test Instance getall subnet InterfaceStatusProvisioning status success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			queryStatus:               cutil.GetPtr(cdbm.InterfaceStatusProvisioning),
			expectedCount:             20,
			expectedTotal:             25,
			orderBy:                   cutil.GetPtr("CREATED_ASC"),
			expectSubnet:              true,
			expectedFirstSubnetID:     subnets[0].ID.String(),
			expectedFirstSubnetName:   cutil.GetPtr(subnets[0].Name),
			expectedFirstInstanceName: cutil.GetPtr(inst1.Name),
			wantErr:                   false,
		},
		{
			name: "test Instance getall subnet BadStatus status success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusBadRequest,
			},
			queryStatus:   cutil.GetPtr("BadStatus"),
			expectedCount: 0,
			expectedTotal: 0,
			wantErr:       false,
		},
		{
			name: "test Instance Interface endpoint rejects top-level Interface filters",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusBadRequest,
			},
			query:   url.Values{"crossTenant": {"true"}},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := GetAllInstanceInterfaceHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			q := req.URL.Query()
			for key, values := range tt.query {
				for _, value := range values {
					q.Add(key, value)
				}
			}
			if tt.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tt.queryIncludeRelations1)
			}
			if tt.queryStatus != nil {
				q.Add("status", *tt.queryStatus)
			}
			if tt.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tt.queryIncludeRelations2)
			}
			if tt.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tt.pageNumber))
			}
			if tt.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tt.pageSize))
			}
			if tt.orderBy != nil {
				q.Set("orderBy", *tt.orderBy)
			}
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/instance/%v/subnet", tt.args.reqOrg, tt.args.reqInstanceID))
			ec.SetParamNames("orgName", "instanceId")
			ec.SetParamValues(tt.args.reqOrg, tt.args.reqInstanceID)
			ec.Set("user", tt.args.reqUser)

			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("GetAllInstanceInterfaceHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("GetAllInstanceInterfaceHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusOK {
				return
			}

			rst := []model.APIInterface{}
			serr := json.Unmarshal(rec.Body.Bytes(), &rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, tt.expectedCount, len(rst))
			if tt.expectSubnet && tt.expectedFirstSubnetID != "" && len(rst) > 0 {
				assert.Equal(t, tt.expectedFirstSubnetID, *rst[0].SubnetID)
			}

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err := json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			assert.Equal(t, tt.expectedTotal, pr.Total)

			if len(rst) > 0 {
				assert.Equal(t, rst[0].InstanceID, tt.args.reqInstance.ID.String())
			}

			if tt.queryIncludeRelations1 != nil || tt.queryIncludeRelations2 != nil {
				if tt.expectSubnet && tt.expectedFirstSubnetName != nil {
					assert.Equal(t, *tt.expectedFirstSubnetName, rst[0].Subnet.Name)
				}

				if tt.expectedFirstInstanceName != nil {
					assert.Equal(t, *tt.expectedFirstInstanceName, rst[0].Instance.Name)
				}
			} else {
				if len(rst) > 0 {
					assert.Nil(t, rst[0].Instance)
					if tt.expectSubnet {
						assert.Nil(t, rst[0].Subnet)
					}
				}
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetAllInterfaceHandler_Handle(t *testing.T) {
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
	ownerSubnet := testInstanceBuildSubnet(t, dbSession, "Owner Subnet", ownerTenant, ownerVpc, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, ownerUser)

	ifcDAO := cdbm.NewInterfaceDAO(dbSession)
	ownerSubnetInterface, err := ifcDAO.Create(ctx, nil, cdbm.InterfaceCreateInput{
		InstanceID: ownerInstance.ID,
		SubnetID:   &ownerSubnet.ID,
		IsPhysical: true,
		Status:     cdbm.InterfaceStatusReady,
		CreatedBy:  ownerUser.ID,
	})
	require.NoError(t, err)
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
	requestedIPAddress := "192.0.2.10"
	routingProfile := &cdbm.InterfaceInlineRoutingProfile{AllowedAnycastPrefixes: []string{"192.0.2.0/24"}}
	crossTenantDirect, err := ifcDAO.Create(ctx, nil, cdbm.InterfaceCreateInput{
		InstanceID:           consumerInstance.ID,
		VpcID:                &ownerVpc.ID,
		Device:               &device,
		DeviceInstance:       &deviceInstance,
		RequestedIpAddress:   &requestedIPAddress,
		InlineRoutingProfile: routingProfile,
		Status:               cdbm.InterfaceStatusPending,
		CreatedBy:            consumerUser.ID,
	})
	require.NoError(t, err)
	macAddress := "02:00:00:00:00:01"
	ipAddresses := []string{"192.0.2.10"}
	ipPrefixes := []string{"192.0.2.0/24"}
	crossTenantDirect, err = ifcDAO.Update(ctx, nil, cdbm.InterfaceUpdateInput{
		InterfaceID: crossTenantDirect.ID,
		MacAddress:  &macAddress,
		IpAddresses: ipAddresses,
		IPPrefixes:  ipPrefixes,
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
	ownerOnConsumerVpc, err := ifcDAO.Create(ctx, nil, cdbm.InterfaceCreateInput{
		InstanceID: ownerInstance.ID,
		VpcID:      &consumerVpc.ID,
		Status:     cdbm.InterfaceStatusReady,
		CreatedBy:  ownerUser.ID,
	})
	require.NoError(t, err)

	handler := NewGetAllInterfaceHandler(dbSession, &tmocks.Client{}, common.GetTestConfig(), nil)
	ownerTenantID := ownerTenant.ID.String()
	consumerTenantID := consumerTenant.ID.String()
	type tenantOwnershipExpectation struct {
		instanceTenantID *string
		vpcTenantID      *string
	}
	checkInterfaces := func(wantIDs []string, wantTenantIDs map[string]tenantOwnershipExpectation, pageSize int, checkDetails bool) func(*httptest.ResponseRecorder) {
		return func(rec *httptest.ResponseRecorder) {
			var interfaces []model.APIInterface
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &interfaces))
			gotIDs := make([]string, 0, len(interfaces))
			interfacesByID := make(map[string]model.APIInterface, len(interfaces))
			for _, apiInterface := range interfaces {
				gotIDs = append(gotIDs, apiInterface.ID)
				interfacesByID[apiInterface.ID] = apiInterface
			}
			assert.ElementsMatch(t, wantIDs, gotIDs)
			var rawInterfaces []map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &rawInterfaces))
			for _, rawInterface := range rawInterfaces {
				assert.Contains(t, rawInterface, "instanceTenantId")
				assert.Contains(t, rawInterface, "vpcTenantId")
			}
			for interfaceID, tenantIDs := range wantTenantIDs {
				assert.Equal(t, tenantIDs.instanceTenantID, interfacesByID[interfaceID].InstanceTenantID)
				assert.Equal(t, tenantIDs.vpcTenantID, interfacesByID[interfaceID].VpcTenantID)
			}

			if checkDetails {
				crossTenantInterface := interfacesByID[crossTenantDirect.ID.String()]
				assert.Equal(t, &macAddress, crossTenantInterface.MacAddress)
				assert.Equal(t, ipAddresses, crossTenantInterface.IPAddresses)
				assert.Equal(t, ipPrefixes, crossTenantInterface.IPPrefixes)
				assert.Equal(t, &requestedIPAddress, crossTenantInterface.RequestedIpAddress)
				require.NotNil(t, crossTenantInterface.InlineRoutingProfile)
				assert.Equal(t, routingProfile.AllowedAnycastPrefixes, crossTenantInterface.InlineRoutingProfile.AllowedAnycastPrefixes)
			}

			var pageResponse pagination.PageResponse
			require.NoError(t, json.Unmarshal([]byte(rec.Header().Get(pagination.ResponseHeaderName)), &pageResponse))
			assert.Equal(t, len(wantIDs), pageResponse.Total)
			assert.Equal(t, pageSize, pageResponse.PageSize)
		}
	}

	// checkRelations asserts which Interfaces expand their Instance and VPC Prefix relations.
	checkRelations := func(wantInstance map[string]bool, wantVpcPrefix map[string]bool) func(*httptest.ResponseRecorder) {
		return func(rec *httptest.ResponseRecorder) {
			var interfaces []model.APIInterface
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &interfaces))
			interfacesByID := make(map[string]model.APIInterface, len(interfaces))
			for _, apiInterface := range interfaces {
				interfacesByID[apiInterface.ID] = apiInterface
			}
			for interfaceID, want := range wantInstance {
				require.Contains(t, interfacesByID, interfaceID)
				assert.Equal(t, want, interfacesByID[interfaceID].Instance != nil, "Instance relation on %s", interfaceID)
			}
			for interfaceID, want := range wantVpcPrefix {
				require.Contains(t, interfacesByID, interfaceID)
				assert.Equal(t, want, interfacesByID[interfaceID].VpcPrefix != nil, "VPC Prefix relation on %s", interfaceID)
			}
		}
	}
	relations := []string{cdbm.InstanceRelationName, cdbm.VpcPrefixRelationName}

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
			check: checkInterfaces(
				[]string{ownerSubnetInterface.ID.String(), ownerDirect.ID.String(), ownerOnConsumerVpc.ID.String(), crossTenantDirect.ID.String(), crossTenantPrefix.ID.String()},
				map[string]tenantOwnershipExpectation{
					ownerSubnetInterface.ID.String(): {},
					ownerDirect.ID.String():          {},
					ownerOnConsumerVpc.ID.String():   {instanceTenantID: &ownerTenantID, vpcTenantID: &consumerTenantID},
					crossTenantDirect.ID.String():    {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
					crossTenantPrefix.ID.String():    {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
				},
				100,
				true,
			),
		},
		{
			name:       "cross-Tenant filter includes both ownership directions",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"crossTenant": {"true"}},
			statusCode: http.StatusOK,
			check: checkInterfaces(
				[]string{ownerOnConsumerVpc.ID.String(), crossTenantDirect.ID.String(), crossTenantPrefix.ID.String()},
				map[string]tenantOwnershipExpectation{
					ownerOnConsumerVpc.ID.String(): {instanceTenantID: &ownerTenantID, vpcTenantID: &consumerTenantID},
					crossTenantDirect.ID.String():  {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
					crossTenantPrefix.ID.String():  {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
				},
				cdbp.DefaultLimit,
				false,
			),
		},
		{
			name:       "cross-Tenant filter returns only other Tenants on owned VPCs",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"crossTenant": {"true"}, "vpcId": {ownerVpc.ID.String()}},
			statusCode: http.StatusOK,
			check: checkInterfaces(
				[]string{crossTenantDirect.ID.String(), crossTenantPrefix.ID.String()},
				map[string]tenantOwnershipExpectation{
					crossTenantDirect.ID.String(): {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
					crossTenantPrefix.ID.String(): {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
				},
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
			check: checkInterfaces(
				[]string{ownerSubnetInterface.ID.String(), ownerDirect.ID.String()},
				map[string]tenantOwnershipExpectation{
					ownerSubnetInterface.ID.String(): {},
					ownerDirect.ID.String():          {},
				},
				cdbp.DefaultLimit,
				false,
			),
		},
		{
			name:       "Instance Tenant filter is constrained by visibility",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"instanceTenantId": {consumerTenant.ID.String()}},
			statusCode: http.StatusOK,
			check: checkInterfaces(
				[]string{crossTenantDirect.ID.String(), crossTenantPrefix.ID.String()},
				map[string]tenantOwnershipExpectation{
					crossTenantDirect.ID.String(): {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
					crossTenantPrefix.ID.String(): {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
				},
				cdbp.DefaultLimit,
				false,
			),
		},
		{
			name:       "VPC Tenant filter includes direct and explicit-prefix Interfaces",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"vpcTenantId": {ownerTenant.ID.String()}},
			statusCode: http.StatusOK,
			check: checkInterfaces(
				[]string{ownerDirect.ID.String(), crossTenantDirect.ID.String(), crossTenantPrefix.ID.String()},
				map[string]tenantOwnershipExpectation{
					ownerDirect.ID.String():       {},
					crossTenantDirect.ID.String(): {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
					crossTenantPrefix.ID.String(): {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
				},
				cdbp.DefaultLimit,
				false,
			),
		},
		{
			name:       "caller sees all own Instance Interfaces across VPC ownership",
			org:        consumerOrg,
			user:       consumerUser,
			statusCode: http.StatusOK,
			check: checkInterfaces(
				[]string{crossTenantDirect.ID.String(), crossTenantPrefix.ID.String(), consumerOwn.ID.String(), ownerOnConsumerVpc.ID.String()},
				map[string]tenantOwnershipExpectation{
					crossTenantDirect.ID.String():  {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
					crossTenantPrefix.ID.String():  {instanceTenantID: &consumerTenantID, vpcTenantID: &ownerTenantID},
					consumerOwn.ID.String():        {},
					ownerOnConsumerVpc.ID.String(): {instanceTenantID: &ownerTenantID, vpcTenantID: &consumerTenantID},
				},
				cdbp.DefaultLimit,
				false,
			),
		},
		{
			name:       "other Tenant VPC filter returns caller-owned cross-Tenant Interface",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"vpcId": {consumerVpc.ID.String()}},
			statusCode: http.StatusOK,
			check: checkInterfaces(
				[]string{ownerOnConsumerVpc.ID.String()},
				map[string]tenantOwnershipExpectation{
					ownerOnConsumerVpc.ID.String(): {instanceTenantID: &ownerTenantID, vpcTenantID: &consumerTenantID},
				},
				cdbp.DefaultLimit,
				false,
			),
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
			name: "cross-Tenant and Instance Tenant filters compose",
			org:  ownerOrg,
			user: ownerUser,
			query: url.Values{
				"crossTenant":      {"true"},
				"instanceTenantId": {ownerTenant.ID.String()},
			},
			statusCode: http.StatusOK,
			check: checkInterfaces(
				[]string{ownerOnConsumerVpc.ID.String()},
				map[string]tenantOwnershipExpectation{
					ownerOnConsumerVpc.ID.String(): {instanceTenantID: &ownerTenantID, vpcTenantID: &consumerTenantID},
				},
				cdbp.DefaultLimit,
				false,
			),
		},
		{
			name:       "includeRelation expands another Tenant's Instance",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"includeRelation": relations},
			statusCode: http.StatusOK,
			check: checkRelations(
				map[string]bool{ownerDirect.ID.String(): true, crossTenantDirect.ID.String(): true},
				map[string]bool{crossTenantPrefix.ID.String(): true},
			),
		},
		{
			name:       "includeRelation expands another Tenant's VPC Prefix",
			org:        consumerOrg,
			user:       consumerUser,
			query:      url.Values{"includeRelation": relations},
			statusCode: http.StatusOK,
			check: checkRelations(
				map[string]bool{crossTenantPrefix.ID.String(): true},
				map[string]bool{crossTenantPrefix.ID.String(): true},
			),
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
			name:       "invalid VPC Tenant ID is rejected",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"vpcTenantId": {"not-a-uuid"}},
			statusCode: http.StatusBadRequest,
		},
		{
			name:       "empty VPC ID is rejected",
			org:        ownerOrg,
			user:       ownerUser,
			query:      url.Values{"vpcId": {""}},
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

func TestNewGetAllInterfaceHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want GetAllInterfaceHandler
	}{
		{
			name: "test GetAllInterfaceHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetAllInterfaceHandler{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetAllInterfaceHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewGetAllInterfaceHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}
