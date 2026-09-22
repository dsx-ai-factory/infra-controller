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

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	tmocks "go.temporal.io/sdk/mocks"
)

func setupSessionAndInfrastructureProviderTables(t *testing.T) *cdb.Session {
	// Set up DB
	dbSession := cdbu.GetTestDBSession(t, false)

	// Create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	if err != nil {
		t.Fatal(err)
	}

	// Create user table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	if err != nil {
		t.Fatal(err)
	}

	return dbSession
}

func TestCreateInfrastructureProviderHandler_Handle(t *testing.T) {
	ctx := context.Background()
	// Set up DB
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}
	tnu := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)

	cfg := common.GetTestConfig()

	type fields struct {
		dbSession *cdb.Session
		tc        *tmocks.Client
		cfg       *config.Config
	}

	type args struct {
		org  string
		user *cdbm.User
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name           string
		fields         fields
		args           args
		wantStatusCode int
	}{
		{
			name: "create infrastructure provider failure, endpoint deprecated",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				user: ipu,
			},
			wantStatusCode: http.StatusBadRequest,
		},
		{
			name: "create infrastructure provider failure, user doesn't have the right role",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg,
				user: tnu,
			},
			wantStatusCode: http.StatusForbidden,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ciph := CreateInfrastructureProviderHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := ciph.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.wantStatusCode, rec.Code)
		})
	}
}

func TestGetCurrentInfrastructureProviderHandler_Handle(t *testing.T) {
	ctx := context.Background()
	// Set up DB
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipOrg := "test-provider-org"
	ipu := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ipuv := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipViewerRoles)

	ipOrg2 := "test-provider-org-2"
	ipu2 := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg2, ipRoles)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}
	tnu := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)

	// Add infrastructure provider entry
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	cfg := common.GetTestConfig()

	type fields struct {
		dbSession *cdb.Session
		tc        *tmocks.Client
		cfg       *config.Config
	}

	type args struct {
		org  string
		user *cdbm.User
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                       string
		fields                     fields
		args                       args
		wantInfrastructureProvider *cdbm.InfrastructureProvider
		wantStatusCode             int
		verifyChildSpanner         bool
	}{
		{
			name: "get current Infrastructure Provider success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				user: ipu,
			},
			wantInfrastructureProvider: ip,
			wantStatusCode:             http.StatusOK,
		},
		{
			name: "get current Infrastructure Provider success when user has Provider viwer role",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				user: ipuv,
			},
			wantInfrastructureProvider: ip,
			wantStatusCode:             http.StatusOK,
		},
		{
			name: "get current Infrastructure Provider success, auto-created when it does not exist",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg2,
				user: ipu2,
			},
			wantStatusCode:     http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get current Infrastructure Provider failure, user doesn't have the right role",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg,
				user: tnu,
			},
			wantStatusCode: http.StatusForbidden,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gciph := GetCurrentInfrastructureProviderHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := gciph.Handle(ec)
			assert.NoError(t, err)

			require.Equal(t, tt.wantStatusCode, rec.Code)
			if tt.wantStatusCode != http.StatusOK {
				return
			}

			rip := &model.APIInfrastructureProvider{}

			err = json.Unmarshal(rec.Body.Bytes(), rip)
			assert.NoError(t, err)

			assert.NotEmpty(t, rip.ID)
			if tt.wantInfrastructureProvider != nil {
				assert.Equal(t, rip.ID, tt.wantInfrastructureProvider.ID.String())
				assert.Equal(t, rip.Org, tt.wantInfrastructureProvider.Org)
				assert.Equal(t, *rip.OrgDisplayName, tt.wantInfrastructureProvider.Org)
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetCurrentInfrastructureProviderStatsHandler_Handle(t *testing.T) {
	ctx := context.Background()
	// Set up DB
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-provider-org-1"
	ipOrg2 := "test-provider-org-2"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu1 := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg1, ipRoles)
	ipu2 := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg2, ipRoles)

	// Add infrastructure provider entry
	ip1 := common.TestBuildInfrastructureProvider(t, dbSession, "test-provider-1", ipOrg1, ipu1)
	assert.NotNil(t, ip1)
	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "test-provider-1", ipOrg2, ipu2)
	assert.NotNil(t, ip2)

	site1 := testIPBlockBuildSite(t, dbSession, ip1, "testSite", cdbm.SiteStatusRegistered, true, ipu1)
	assert.NotNil(t, site1)

	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, ipu2)
	assert.NotNil(t, site2)

	tnOrg1 := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}
	tnu1 := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg1, tnRoles)
	tn1 := testVPCBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)
	assert.NotNil(t, tn1)

	cfg := common.GetTestConfig()

	type fields struct {
		dbSession *cdb.Session
		tc        *tmocks.Client
		cfg       *config.Config
	}

	type args struct {
		org  string
		user *cdbm.User
	}

	// Build IPBlock

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "test1", site1, ip1, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu1)
	assert.NotNil(t, ipb1)

	ipb2 := testIPBlockBuildIPBlock(t, dbSession, "test2", site1, ip1, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.2.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu2)
	assert.NotNil(t, ipb2)
	// This private Tenant SitePrefix belongs to the provider but must not change
	// its generic IPBlock total.
	testIPBlockBuildTenantSitePrefix(t, dbSession, "private-site-prefix", site1, ip1, tn1, "192.168.3.0", 24, cdbm.IPBlockStatusReady, ipu1)

	// Build Machine
	m1 := testMachineBuildMachine(t, dbSession, ip1.ID, site1.ID, nil, cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusInitializing)
	assert.NotNil(t, m1)

	m2 := testMachineBuildMachine(t, dbSession, ip1.ID, site1.ID, nil, cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusInitializing)
	assert.NotNil(t, m2)

	// Build Tenant Account
	ta11 := testTenantAccountBuildTenantAccount(t, dbSession, uuid.New().String(), ip1, tn1, tn1.Org, cdbm.TenantAccountStatusInvited, ipu1.ID, uuid.Nil)
	assert.NotNil(t, ta11)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		reqCurrentIP       *cdbm.InfrastructureProvider
		reqCurrentUser     *cdbm.User
		reqEmpty           bool
		reqMachineCount    int
		reqIPBlockCount    int
		reqTaCount         int
		verifyChildSpanner bool
	}{
		{
			name: "test infrastructure provider get current stats API endpoint return stats",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg1,
				user: ipu1,
			},
			wantErr:         false,
			reqEmpty:        false,
			reqMachineCount: 2,
			reqIPBlockCount: 2,
			reqTaCount:      1,
			reqCurrentIP:    ip1,
			reqCurrentUser:  ipu1,
		},
		{
			name: "test infrastructure provider get current stats API endpoint return no stats",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg2,
				user: ipu2,
			},
			wantErr:            false,
			reqEmpty:           true,
			reqMachineCount:    0,
			reqIPBlockCount:    0,
			reqTaCount:         0,
			reqCurrentIP:       ip2,
			reqCurrentUser:     ipu2,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		e := echo.New()
		req := httptest.NewRequest(http.MethodGet, "/", nil)
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		rec := httptest.NewRecorder()

		ec := e.NewContext(req, rec)
		ec.SetParamNames("orgName")
		ec.SetParamValues(tt.args.org)
		ec.Set("user", tt.reqCurrentUser)

		ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
		ec.SetRequest(ec.Request().WithContext(ctx))

		gctnsh := GetCurrentInfrastructureProviderStatsHandler{
			dbSession: tt.fields.dbSession,
			tc:        tt.fields.tc,
			cfg:       tt.fields.cfg,
		}
		if err := gctnsh.Handle(ec); (err != nil) != tt.wantErr {
			t.Errorf("GetCurrentInfrastructureProviderStatsHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
		}

		assert.Equal(t, http.StatusOK, rec.Code)

		fmt.Printf("%v", rec.Body.String())

		rst := &model.APIInfrastructureProviderStats{}
		serr := json.Unmarshal(rec.Body.Bytes(), rst)
		if serr != nil {
			t.Fatal(serr)
		}

		assert.Equal(t, rst.IPBlock.Total, tt.reqIPBlockCount)
		assert.Equal(t, rst.Machine.Total, tt.reqMachineCount)
		assert.Equal(t, rst.TenantAccount.Total, tt.reqTaCount)

		if !tt.reqEmpty {
			// IPBlock assert
			assert.Equal(t, rst.IPBlock.Pending, 1)
			assert.Equal(t, rst.IPBlock.Ready, 1)

			// Machine assert
			assert.Equal(t, rst.Machine.Initializing, 2)

			// TenantAccount assert
			assert.Equal(t, rst.TenantAccount.Invited, 1)
		}

		if tt.verifyChildSpanner {
			span := oteltrace.SpanFromContext(ec.Request().Context())
			assert.True(t, span.SpanContext().IsValid())
		}
	}
}

func TestUpdateInfrastructureProviderHandler_Handle(t *testing.T) {
	ctx := context.Background()
	// Set up DB
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}
	tnu := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)

	cfg := common.GetTestConfig()

	type fields struct {
		dbSession *cdb.Session
		tc        *tmocks.Client
		cfg       *config.Config
	}

	type args struct {
		org  string
		user *cdbm.User
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name           string
		fields         fields
		args           args
		wantStatusCode int
	}{
		{
			name: "update infrastructure provider failure, endpoint deprecated",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				user: ipu,
			},
			wantStatusCode: http.StatusBadRequest,
		},
		{
			name: "update infrastructure provider failure, user doesn't have the right role",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg,
				user: tnu,
			},
			wantStatusCode: http.StatusForbidden,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			uciph := UpdateCurrentInfrastructureProviderHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPatch, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.Set("user", tt.args.user)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := uciph.Handle(ec)
			require.NoError(t, err)

			require.Equal(t, tt.wantStatusCode, rec.Code)
		})
	}
}
