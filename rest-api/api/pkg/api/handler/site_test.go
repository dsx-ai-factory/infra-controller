// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	sutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	csmtypes "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/types"
	"github.com/golang/mock/gomock"
	"github.com/google/uuid"
	"github.com/gorilla/mux"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	oteltrace "go.opentelemetry.io/otel/trace"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	tOperatorv1 "go.temporal.io/api/operatorservice/v1"
	tosv1mock "go.temporal.io/api/operatorservicemock/v1"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
)

func testUpdateSite(t *testing.T, dbSession *cdb.Session, site *cdbm.Site) *cdbm.Site {
	_, err := dbSession.DB.NewUpdate().Where("id = ?", site.ID).Model(site).Exec(context.Background())
	assert.Nil(t, err)
	return site
}

type testCsm struct {
	l        net.Listener
	srv      *httptest.Server
	errCode  int
	forceErr bool
}

func (csm *testCsm) setup(t *testing.T) {
	l, err := net.Listen("tcp", "localhost:0")
	require.NoError(t, err)
	csm.l = l

	if csm.errCode == 0 {
		csm.errCode = http.StatusInternalServerError
	}

	rtr := mux.NewRouter()
	rtr.HandleFunc("/v1/site", func(w http.ResponseWriter, r *http.Request) {
		if csm.forceErr {
			http.Error(w, "forced error", csm.errCode)
		}
	})
	rtr.HandleFunc("/v1/site/{id}", func(w http.ResponseWriter, r *http.Request) {
		if csm.forceErr {
			http.Error(w, "forced error", csm.errCode)
			return
		}
		if r.Method == http.MethodGet {
			resp := &csmtypes.SiteGetResponse{
				OTP:       "test-otp",
				OTPExpiry: time.Now().Add(24 * time.Hour).String(),
			}

			c, err := json.Marshal(resp)
			require.NoError(t, err)
			w.Header().Set("Content-Type", "application/json")
			_, err = w.Write(c)
			require.NoError(t, err)
		}
	})
	rtr.HandleFunc("/v1/site/roll/{id}", func(w http.ResponseWriter, r *http.Request) {
		if csm.forceErr {
			http.Error(w, "forced error", http.StatusNotFound)
		}
	})

	csm.srv = httptest.NewUnstartedServer(rtr)
	csm.srv.Listener = l
	csm.srv.StartTLS()
}

func (csm *testCsm) getURL() string {
	return fmt.Sprintf("https://%s/v1/site", csm.l.Addr().String())
}

func (csm *testCsm) teardown() {
	csm.srv.Close()
}

func testSiteInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

// reset the tables needed for Site tests
func testSiteSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create TenantSite table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantSite)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InstanceType)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IPBlock)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil))
	assert.Nil(t, err)
	// create Status Details table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	// create NVLinkLogicalPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkLogicalPartition)(nil))
	assert.Nil(t, err)
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create VPC table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
}

func testSiteBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
	ipDAO := cdbm.NewInfrastructureProviderDAO(dbSession)

	ip, err := ipDAO.Create(context.Background(), nil, cdbm.InfrastructureProviderCreateInput{
		Name:        name,
		DisplayName: cutil.GetPtr("Test Infrastructure Provider"),
		Org:         org,
		CreatedBy:   user.ID,
	})
	assert.Nil(t, err)

	return ip
}

func testSiteBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, status string, user *cdbm.User, location *cdbm.SiteLocation, contact *cdbm.SiteContact, config *cdbm.SiteConfig) *cdbm.Site {
	stDAO := cdbm.NewSiteDAO(dbSession)

	if config == nil {
		config = &cdbm.SiteConfig{}
	}

	st, err := stDAO.Create(context.Background(), nil,
		cdbm.SiteCreateInput{
			Name:                          name,
			DisplayName:                   cutil.GetPtr("Test Site"),
			Description:                   cutil.GetPtr("Test Site Description"),
			Org:                           ip.Org,
			InfrastructureProviderID:      ip.ID,
			SiteControllerVersion:         cutil.GetPtr("1.0.0"),
			SiteAgentVersion:              cutil.GetPtr("1.0.0"),
			RegistrationToken:             cutil.GetPtr("1234-5678-9012-3456"),
			RegistrationTokenExpiration:   cutil.GetPtr(cdb.GetCurTime()),
			IsInfinityEnabled:             false,
			SerialConsoleHostname:         cutil.GetPtr("nico.acme.com"),
			IsSerialConsoleEnabled:        true,
			SerialConsoleIdleTimeout:      cutil.GetPtr(30),
			SerialConsoleMaxSessionLength: cutil.GetPtr(60),
			Location:                      location,
			Contact:                       contact,
			Config:                        *config,
			Status:                        status,
			CreatedBy:                     user.ID,
		})
	assert.Nil(t, err)

	return st
}

func testSiteBuildTenant(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.Tenant {
	tnDAO := cdbm.NewTenantDAO(dbSession)

	tn, err := tnDAO.Create(context.Background(), nil, cdbm.TenantCreateInput{
		Name:        name,
		DisplayName: cutil.GetPtr("Test Tenant"),
		Org:         org,
		CreatedBy:   user.ID,
	})
	assert.Nil(t, err)

	return tn
}

func testSiteBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, org string, roles []string) *cdbm.User {
	uDAO := cdbm.NewUserDAO(dbSession)

	u, err := uDAO.Create(
		context.Background(),
		nil,
		cdbm.UserCreateInput{
			AuxiliaryID: nil,
			StarfleetID: &starfleetID,
			Email:       cutil.GetPtr("jdoe@test.com"),
			FirstName:   cutil.GetPtr("John"),
			LastName:    cutil.GetPtr("Doe"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       roles,
				},
			},
		},
	)
	assert.Nil(t, err)

	return u
}

func testSiteBuildAllocation(t *testing.T, dbSession *cdb.Session, st *cdbm.Site, tn *cdbm.Tenant, name string, user *cdbm.User) *cdbm.Allocation {
	alDAO := cdbm.NewAllocationDAO(dbSession)

	createInput := cdbm.AllocationCreateInput{
		Name:                     name,
		Description:              cutil.GetPtr("Test Allocation Description"),
		InfrastructureProviderID: st.InfrastructureProviderID,
		TenantID:                 tn.ID,
		SiteID:                   st.ID,
		Status:                   cdbm.AllocationStatusPending,
		CreatedBy:                user.ID,
	}
	al, err := alDAO.Create(context.Background(), nil, createInput)
	assert.Nil(t, err)

	return al
}

func testSiteBuildMachine(t *testing.T, dbSession *cdb.Session, ip uuid.UUID, site uuid.UUID, status string) *cdbm.Machine {
	return testSiteBuildMachineWithID(t, dbSession, ip, site, uuid.NewString(), status)
}

func testSiteBuildMachineWithID(t *testing.T, dbSession *cdb.Session, ip uuid.UUID, site uuid.UUID, machineID string, status string) *cdbm.Machine {
	m := &cdbm.Machine{
		ID:                       machineID,
		InfrastructureProviderID: ip,
		SiteID:                   site,
		ControllerMachineID:      machineID,
		Metadata:                 nil,
		DefaultMacAddress:        cutil.GetPtr("00:1B:44:11:3A:B7"),
		Status:                   status,
	}
	_, err := dbSession.DB.NewInsert().Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

func TestCreateSiteHandler_Handle(t *testing.T) {
	ctx := context.Background()
	tcsm := &testCsm{}
	tcsm.setup(t)
	defer tcsm.teardown()

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testSiteSetupSchema(t, dbSession)

	org := "test-org"
	orgRoles := []string{authz.ProviderAdminRole}

	ipu := testSiteBuildUser(t, dbSession, "test123", org, orgRoles)
	ip := testSiteBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, ipu)

	stcr1 := &model.APISiteCreateRequest{
		Name:                  "Test Site 1",
		Description:           cutil.GetPtr("Test Site Description"),
		SerialConsoleHostname: cutil.GetPtr("acme.com"),
	}
	stcrJSON1, _ := json.Marshal(stcr1)

	stcr3 := &model.APISiteCreateRequest{
		Name:        "Test Site 3",
		Description: cutil.GetPtr("Test Site Description"),
	}
	stcrJSON3, _ := json.Marshal(stcr3)
	stcr4 := &model.APISiteCreateRequest{
		Name:        "Test Site 4",
		Description: cutil.GetPtr("Test Site Description"),
	}
	stcrJSON4, _ := json.Marshal(stcr4)
	stcr5 := &model.APISiteCreateRequest{
		Name:        "Test Site 5",
		Description: cutil.GetPtr("Test Site Description"),
		Location: &model.APISiteLocation{
			City:    "San Jose",
			State:   "CA",
			Country: "USA",
		},
	}
	stcrJSON5, _ := json.Marshal(stcr5)
	stcr6 := &model.APISiteCreateRequest{
		Name:        "Test Site 6",
		Description: cutil.GetPtr("Test Site Description"),
		Contact: &model.APISiteContact{
			Email: "johndoe@nvidia.com",
		},
	}
	stcrJSON6, _ := json.Marshal(stcr6)

	cfg := common.GetTestConfig()

	cfg.SetSiteManagerEnabled(true)
	cfg.SetSiteManagerEndpoint(tcsm.getURL())

	tc := &tmocks.Client{}
	tnc := &tmocks.NamespaceClient{}

	tnc.Mock.On("Register", mock.Anything, mock.AnythingOfType("*workflowservice.RegisterNamespaceRequest")).Return(nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		tnc       temporalClient.NamespaceClient
		cfg       *config.Config
	}

	tests := []struct {
		name                          string
		fields                        fields
		wantErr                       bool
		body                          string
		expectedName                  string
		expectedDescription           *string
		expectedSerialConsoleHostname *string
		respCode                      int
		siteMgrErr                    bool
		siteMgrDisabled               bool
		verifyChildSpanner            bool
		expectedLocation              *model.APISiteLocation
		expectedContact               *model.APISiteContact
	}{
		{
			name: "OK Site create API endpoint",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				tnc:       tnc,
				cfg:       cfg,
			},
			wantErr:                       false,
			body:                          string(stcrJSON1),
			expectedName:                  stcr1.Name,
			expectedDescription:           stcr1.Description,
			expectedSerialConsoleHostname: stcr1.SerialConsoleHostname,
		},
		{
			name: "Error Site create API endpoint, Site with name exists",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				tnc:       tnc,
				cfg:       cfg,
			},
			wantErr:  true,
			respCode: http.StatusConflict,
			body:     string(stcrJSON1),
		},
		{
			name: "Error in Site create API when site manager return error",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				tnc:       tnc,
				cfg:       cfg,
			},
			wantErr:             true,
			body:                string(stcrJSON4),
			expectedName:        stcr4.Name,
			expectedDescription: stcr4.Description,
			siteMgrErr:          true,
			respCode:            http.StatusInternalServerError,
		},
		{
			name: "OK Site create API endpoint, sitemgr disabled",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				tnc:       tnc,
				cfg:       cfg,
			},
			wantErr:             false,
			body:                string(stcrJSON3),
			expectedName:        stcr3.Name,
			expectedDescription: stcr3.Description,
			siteMgrDisabled:     true,
			verifyChildSpanner:  true,
		},
		{
			name: "create site with location",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				tnc:       tnc,
				cfg:       cfg,
			},
			wantErr:          false,
			body:             string(stcrJSON5),
			expectedLocation: stcr5.Location,
			siteMgrDisabled:  true,
			expectedName:     stcr5.Name,
		},
		{
			name: "create site with contact",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				tnc:       tnc,
				cfg:       cfg,
			},
			wantErr:         false,
			body:            string(stcrJSON6),
			expectedContact: stcr6.Contact,
			siteMgrDisabled: true,
			expectedName:    stcr6.Name,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tcsm.forceErr = tt.siteMgrErr
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(tt.body))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(ip.Org)
			ec.Set("user", ipu)

			csh := NewCreateSiteHandler(tt.fields.dbSession, tt.fields.tc, tt.fields.tnc, tt.fields.cfg)

			if tt.siteMgrDisabled {
				tt.fields.cfg.SetSiteManagerEnabled(false)
				tt.fields.cfg.SetSiteManagerEndpoint("")
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := csh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tt.wantErr, rec.Code != http.StatusCreated)

			if !tt.wantErr {
				assert.Equal(t, http.StatusCreated, rec.Code)

				rst := &model.APISite{}

				serr := json.Unmarshal(rec.Body.Bytes(), rst)
				if serr != nil {
					t.Fatal(serr)
				}

				assert.Equal(t, tt.expectedName, rst.Name)

				if tt.expectedDescription != nil {
					assert.Equal(t, *tt.expectedDescription, *rst.Description)
				}

				if tt.expectedSerialConsoleHostname != nil {
					assert.Equal(t, *tt.expectedSerialConsoleHostname, *rst.SerialConsoleHostname)
				}

				assert.Equal(t, rst.Status, cdbm.SiteStatusPending)
				assert.Equal(t, len(rst.StatusHistory), 1)
				require.NotNil(t, rst.Capabilities)
				assert.True(t, rst.Capabilities.NativeNetworking)
				assert.True(t, rst.Capabilities.NetworkSecurityGroup)
				assert.True(t, rst.Capabilities.Flow)
				assert.False(t, rst.Capabilities.VpcSlaac)
				assert.False(t, rst.Capabilities.DPSPowerManagement)

				createdSiteID, perr := uuid.Parse(rst.ID)
				require.NoError(t, perr)
				createdSite, gerr := cdbm.NewSiteDAO(tt.fields.dbSession).GetByID(
					context.Background(), nil, createdSiteID, nil, false,
				)
				require.NoError(t, gerr)
				require.NotNil(t, createdSite.Config)
				assert.True(t, createdSite.Config.NativeNetworking)
				assert.True(t, createdSite.Config.NetworkSecurityGroup)
				assert.True(t, createdSite.Config.Flow)
				assert.False(t, createdSite.Config.VpcSlaac)

				if !tt.siteMgrDisabled {
					assert.NotNil(t, rst.RegistrationToken)
					assert.NotNil(t, rst.RegistrationTokenExpiration)
				}

				if tt.expectedLocation != nil {
					assert.NotNil(t, rst.Location)
					assert.Equal(t, tt.expectedLocation.City, rst.Location.City)
					assert.Equal(t, tt.expectedLocation.State, rst.Location.State)
					assert.Equal(t, tt.expectedLocation.Country, rst.Location.Country)
				}
				if tt.expectedContact != nil {
					assert.NotNil(t, rst.Contact)
					assert.Equal(t, tt.expectedContact.Email, rst.Contact.Email)
				}
			} else {
				assert.Equal(t, tt.respCode, rec.Code)
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestUpdateSiteHandler_Handle(t *testing.T) {
	ctx := context.Background()
	tcsm := &testCsm{}
	tcsm.setup(t)
	defer tcsm.teardown()

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	testSiteSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testSiteBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := testSiteBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testSiteBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := testSiteBuildTenant(t, dbSession, "test-tenant-1", tnOrg, tnu)

	mOrg := "test-mixed-org"
	mixedRole := []string{authz.ProviderAdminRole, authz.TenantAdminRole}
	mu := testSiteBuildUser(t, dbSession, uuid.NewString(), mOrg, mixedRole)
	mip := testSiteBuildInfrastructureProvider(t, dbSession, "test-mixed-provider", mOrg, mu)
	mtn := testSiteBuildTenant(t, dbSession, "test-mixed-tenant", mOrg, mu)
	mst := testSiteBuildSite(t, dbSession, mip, "test-mixed-site", cdbm.SiteStatusRegistered, mu, nil, nil, nil)
	testSiteBuildAllocation(t, dbSession, mst, mtn, "test-allocation-mixed", mu)
	common.TestBuildTenantSite(t, dbSession, mtn, mst, mu)

	st := testSiteBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)
	st2 := testSiteBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusError, ipu, nil, nil, nil)
	st3 := testSiteBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)
	st4 := testSiteBuildSite(t, dbSession, ip, "test-site-4", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)
	st5 := testSiteBuildSite(t, dbSession, ip, "test-site-5", cdbm.SiteStatusRegistered, ipu, nil, nil, &cdbm.SiteConfig{NativeNetworking: true, NetworkSecurityGroup: true, Flow: true, VpcSlaac: true})
	st6 := testSiteBuildSite(t, dbSession, ip, "test-site-6", cdbm.SiteStatusRegistered, ipu, nil, nil, &cdbm.SiteConfig{NativeNetworking: true, NetworkSecurityGroup: true})
	stPower := testSiteBuildSite(t, dbSession, ip, "test-site-power", cdbm.SiteStatusRegistered, ipu, nil, nil, &cdbm.SiteConfig{})

	common.TestBuildTenantSite(t, dbSession, tn, st6, tnu)
	common.TestBuildVPC(t, dbSession, "test-vpc", ip, tn, st6, nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu)

	testSiteBuildAllocation(t, dbSession, st, tn, "test-allocation", ipu)
	common.TestBuildTenantSite(t, dbSession, tn, st, ipu)

	cfg := common.GetTestConfig()
	cfg.SetSiteManagerEnabled(true)
	cfg.SetSiteManagerEndpoint(tcsm.getURL())

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		site    *cdbm.Site
		org     string
		user    *cdbm.User
		reqData *model.APISiteUpdateRequest
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		query              url.Values
		wantStatus         *string
		respMessage        string
		siteMgrErr         bool
		csmEnabled         bool
		verifyTenantUpdate bool
		verifyChildSpanner bool
		verifyFlow         bool
		verifyVpcSlaac     bool
	}{
		{
			name: "test Site update API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Name:                          cutil.GetPtr("test-site-updated"),
					Description:                   cutil.GetPtr("Test updated description"),
					RenewRegistrationToken:        cutil.GetPtr(true),
					SerialConsoleHostname:         cutil.GetPtr("nico.acme.com"),
					IsSerialConsoleEnabled:        cutil.GetPtr(true),
					SerialConsoleIdleTimeout:      cutil.GetPtr(120),
					SerialConsoleMaxSessionLength: cutil.GetPtr(240),
				},
			},
			csmEnabled:         true,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Site update API endpoint rejects Provider modification of inventory-managed Flow",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st5,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Capabilities: &model.APISiteCapabilitiesUpdateRequest{Flow: cutil.GetPtr(false)},
				},
			},
			csmEnabled:  true,
			wantErr:     true,
			respMessage: model.ErrMsgNotConfigurableByProvider,
			verifyFlow:  true,
		},
		{
			name: "test Site update API endpoint rejects Provider modification of inventory capability",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st5,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Capabilities: &model.APISiteCapabilitiesUpdateRequest{VpcSlaac: cutil.GetPtr(false)},
				},
			},
			csmEnabled:     true,
			wantErr:        true,
			respMessage:    model.ErrMsgNotConfigurableByProvider,
			verifyVpcSlaac: true,
		},
		{
			name: "test Site update API endpoint success modifying capabilities",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st5,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Capabilities: &model.APISiteCapabilitiesUpdateRequest{NativeNetworking: cutil.GetPtr(false)},
				},
			},
			csmEnabled:         true,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Site update API endpoint success enabling DPS power management",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: stPower,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Capabilities: &model.APISiteCapabilitiesUpdateRequest{DPSPowerManagement: cutil.GetPtr(true)},
				},
			},
			csmEnabled:         true,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Site update API endpoint failure when disabling Native Networking while Site has one or more VPCs with virtualization type set to FNN",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st6,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Capabilities: &model.APISiteCapabilitiesUpdateRequest{NativeNetworking: cutil.GetPtr(false)},
				},
			},
			csmEnabled:         true,
			wantErr:            true,
			verifyChildSpanner: false,
		},
		{
			name: "test registration token renewal success for Site in Error state",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st2,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					RenewRegistrationToken: cutil.GetPtr(true),
				},
			},
			csmEnabled: true,
			wantErr:    false,
			wantStatus: cutil.GetPtr(cdbm.SiteStatusPending),
		},
		{
			name: "test registration token renewal success for Site in Registered state",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st3,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					RenewRegistrationToken: cutil.GetPtr(true),
				},
			},
			csmEnabled: true,
			wantErr:    false,
			wantStatus: cutil.GetPtr(cdbm.SiteStatusRegistered),
		},
		{
			name: "test Site update API endpoint failure by Tenant, configuring serial console SSH keys is no longer supported",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st,
				org:  tnOrg,
				user: tnu,
				reqData: &model.APISiteUpdateRequest{
					IsSerialConsoleSSHKeysEnabled: cutil.GetPtr(true),
				},
			},
			csmEnabled:         true,
			wantErr:            true,
			verifyChildSpanner: false,
		},
		{
			name: "test Site update API endpoint failure by Tenant, changing Provider specific attributes not allowed",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st,
				org:  tnOrg,
				user: tnu,
				reqData: &model.APISiteUpdateRequest{
					Name:                   cutil.GetPtr("test-site-updated"),
					IsSerialConsoleEnabled: cutil.GetPtr(true),
				},
			},
			csmEnabled:         true,
			wantErr:            true,
			verifyChildSpanner: false,
		},
		{
			name: "test Site update API endpoint failure by Provider, updating Tenant specific attributes not allowed",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					IsSerialConsoleSSHKeysEnabled: cutil.GetPtr(true),
				},
			},
			csmEnabled:         true,
			wantErr:            true,
			verifyChildSpanner: false,
		},
		{
			name: "test Site update success when user has both Provider and Tenant roles",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: mst,
				org:  mOrg,
				user: mu,
				reqData: &model.APISiteUpdateRequest{
					Description: cutil.GetPtr("Updated Site description"),
				},
			},
			csmEnabled: false,
			wantErr:    false,
		},
		{
			name: "test Site update API fails when name clashes",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Name: cutil.GetPtr("test-site-2"),
				},
			},
			csmEnabled: true,
			wantErr:    true,
		},
		{
			name: "test Site update success with same name",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Name: cutil.GetPtr("test-site"),
				},
			},
			csmEnabled: false,
			wantErr:    false,
		},
		{
			name: "test Site update API endpoint CSM disabled",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Name: cutil.GetPtr("test-site-x"),
				},
			},
			csmEnabled: false,
			wantErr:    false,
		},
		{
			name: "test Site update API endpoint CSM error",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Name:                   cutil.GetPtr("test-site-4"),
					Description:            cutil.GetPtr("Test Site Description Updated"),
					SerialConsoleHostname:  cutil.GetPtr("nico.acme.com"),
					RenewRegistrationToken: cutil.GetPtr(true),
				},
			},
			csmEnabled: true,
			wantErr:    true,
			siteMgrErr: true,
		},
		{
			name: "test Site update API endpoint error, SOL params specified and Site is not Registered",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st2,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					IsSerialConsoleEnabled:        cutil.GetPtr(true),
					SerialConsoleIdleTimeout:      cutil.GetPtr(120),
					SerialConsoleMaxSessionLength: cutil.GetPtr(240),
				},
			},
			wantErr: true,
		},
		{
			name: "test Site update API endpoint success with location",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st4,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Location: &model.APISiteLocation{
						City:    "San Jose",
						State:   "CA",
						Country: "USA",
					},
				},
			},
		},
		{
			name: "test Site update API endpoint success with contact",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				site: st4,
				org:  ipOrg,
				user: ipu,
				reqData: &model.APISiteUpdateRequest{
					Contact: &model.APISiteContact{
						Email: "johndoe@nvidia.com",
					},
				},
			},
		},
	}
	for _, tt := range tests {
		tcsm.forceErr = tt.siteMgrErr
		cfg.SetSiteManagerEnabled(tt.csmEnabled)
		t.Run(tt.name, func(t *testing.T) {
			// Setup echo server/context
			e := echo.New()
			reqJSON, _ := json.Marshal(tt.args.reqData)
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(string(reqJSON)))
			if tt.query != nil {
				req.URL.RawQuery = tt.query.Encode()
			}
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/site/%v", tt.args.org, tt.args.site.ID))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.org, tt.args.site.ID.String())
			ec.Set("user", tt.args.user)

			ush := NewUpdateSiteHandler(tt.fields.dbSession, tt.fields.tc, tt.fields.cfg)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := ush.Handle(ec)
			assert.Nil(t, err)

			if rec.Code != http.StatusOK {
				t.Logf("body: %s", rec.Body.Bytes())
			}
			require.Equal(t, tt.wantErr, rec.Code != http.StatusOK)
			if tt.respMessage != "" {
				assert.Contains(t, rec.Body.String(), tt.respMessage)
			}
			if tt.verifyVpcSlaac {
				storedSite, getErr := cdbm.NewSiteDAO(tt.fields.dbSession).GetByID(ctx, nil, tt.args.site.ID, nil, false)
				require.NoError(t, getErr)
				require.NotNil(t, storedSite.Config)
				assert.True(t, storedSite.Config.VpcSlaac)
			}
			if tt.verifyFlow {
				storedSite, getErr := cdbm.NewSiteDAO(tt.fields.dbSession).GetByID(ctx, nil, tt.args.site.ID, nil, false)
				require.NoError(t, getErr)
				require.NotNil(t, storedSite.Config)
				assert.True(t, storedSite.Config.Flow)
			}

			rst := &model.APISite{}

			if !tt.wantErr {
				serr := json.Unmarshal(rec.Body.Bytes(), rst)
				if serr != nil {
					t.Fatal(serr)
				}

				updated := false

				if tt.verifyTenantUpdate {
					if tt.args.reqData.IsSerialConsoleEnabled != nil {
						// assert.Equal(t, *tt.args.reqData.IsSerialConsoleEnabled, *rst.IsSerialConsoleEnabled)
						updated = true
					}
				} else {
					if tt.args.reqData.Name != nil {
						assert.Equal(t, *tt.args.reqData.Name, rst.Name)
						updated = true
					}

					if tt.args.reqData.Description != nil {
						assert.Equal(t, *tt.args.reqData.Description, *rst.Description)
						updated = true
					}

					if tt.args.reqData.RenewRegistrationToken != nil {
						assert.NotEqual(t, *rst.RegistrationToken, *tt.args.site.RegistrationToken)
						updated = true
					}

					if tt.args.reqData.SerialConsoleHostname != nil {
						assert.Equal(t, *tt.args.reqData.SerialConsoleHostname, *rst.SerialConsoleHostname)
						updated = true
					}

					if tt.args.reqData.IsSerialConsoleEnabled != nil {
						assert.Equal(t, *tt.args.reqData.IsSerialConsoleEnabled, rst.IsSerialConsoleEnabled)
						updated = true
					}

					if tt.args.reqData.SerialConsoleIdleTimeout != nil {
						assert.Equal(t, *tt.args.reqData.SerialConsoleIdleTimeout, *rst.SerialConsoleIdleTimeout)
						updated = true
					}

					if tt.args.reqData.SerialConsoleMaxSessionLength != nil {
						assert.Equal(t, *tt.args.reqData.SerialConsoleMaxSessionLength, *rst.SerialConsoleMaxSessionLength)
						updated = true
					}

					if tt.args.reqData.Location != nil {
						assert.NotNil(t, rst.Location)
						assert.Equal(t, tt.args.reqData.Location.City, rst.Location.City)
						assert.Equal(t, tt.args.reqData.Location.State, rst.Location.State)
						assert.Equal(t, tt.args.reqData.Location.Country, rst.Location.Country)
					}
					if tt.args.reqData.Contact != nil {
						assert.NotNil(t, rst.Contact)
						assert.Equal(t, tt.args.reqData.Contact.Email, rst.Contact.Email)
					}

					if tt.args.reqData.Capabilities != nil {
						if tt.args.reqData.Capabilities.NativeNetworking != nil {
							assert.Equal(t, *tt.args.reqData.Capabilities.NativeNetworking, rst.Capabilities.NativeNetworking)
							updated = true
						} else {
							assert.Equal(t, tt.args.site.Config.NativeNetworking, rst.Capabilities.NativeNetworking)
						}
						if tt.args.reqData.Capabilities.NetworkSecurityGroup != nil {
							assert.Equal(t, *tt.args.reqData.Capabilities.NetworkSecurityGroup, rst.Capabilities.NetworkSecurityGroup)
							updated = true
						} else {
							assert.Equal(t, tt.args.site.Config.NetworkSecurityGroup, rst.Capabilities.NetworkSecurityGroup)
						}
						if tt.args.reqData.Capabilities.DPSPowerManagement != nil {
							assert.Equal(t, *tt.args.reqData.Capabilities.DPSPowerManagement, rst.Capabilities.DPSPowerManagement)
							updated = true
						}
					}
					expectedVpcSlaac := false
					if tt.args.site.Config != nil {
						expectedVpcSlaac = tt.args.site.Config.VpcSlaac
					}
					assert.Equal(t, expectedVpcSlaac, rst.Capabilities.VpcSlaac)
				}

				if updated {
					assert.NotEqual(t, tt.args.site.Updated.String(), rst.Updated.String())
				}

				if tt.wantStatus != nil {
					assert.Equal(t, *tt.wantStatus, rst.Status)
				}
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetSiteHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testSiteSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipvRoles := []string{authz.ProviderViewerRole}

	ipu := testSiteBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ipuv := testSiteBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipvRoles)
	ip := testSiteBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	st := testSiteBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrg3 := "test-tenant-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu1 := testSiteBuildUser(t, dbSession, uuid.NewString(), tnOrg1, tnRoles)
	assert.NotNil(t, tnu1)

	tnu2 := testSiteBuildUser(t, dbSession, uuid.NewString(), tnOrg2, tnRoles)
	assert.NotNil(t, tnu2)

	tnu3 := testSiteBuildUser(t, dbSession, uuid.NewString(), tnOrg3, tnRoles)
	assert.NotNil(t, tnu3)

	tn1 := testSiteBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnu1)
	assert.NotNil(t, tn1)

	tn2 := testSiteBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu2)
	assert.NotNil(t, tn2)

	// Tenant 3 is privileged via Ready TenantAccount.config
	tn3 := testSiteBuildTenant(t, dbSession, "test-tenant-3", tnOrg3, tnu3)
	assert.NotNil(t, tn3)

	ta3 := common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip, &tn3.ID, tnOrg3, cdbm.TenantAccountStatusReady, tnu3)
	assert.NotNil(t, ta3)

	// Tenant 1 has an allocation
	testSiteBuildAllocation(t, dbSession, st, tn1, "test-allocation", ipu)
	ts := common.TestBuildTenantSite(t, dbSession, tn1, st, ipu)

	vOrg1 := "test-provider-org-1"
	vu1 := testSiteBuildUser(t, dbSession, uuid.NewString(), vOrg1, []string{"INVALID_ROLE"})

	vOrg2 := "test-provider-org-2"
	vu2 := testSiteBuildUser(t, dbSession, uuid.NewString(), vOrg2, ipRoles)

	sOrg := "test-service-org"
	sRoles := []string{authz.ProviderAdminRole, authz.TenantAdminRole}
	su := testSiteBuildUser(t, dbSession, uuid.NewString(), sOrg, sRoles)
	sip := testSiteBuildInfrastructureProvider(t, dbSession, "test-service-provider", sOrg, su)
	stn := testSiteBuildTenant(t, dbSession, "test-service-tenant", sOrg, su)

	ss := testSiteBuildSite(t, dbSession, sip, "test-service-site", cdbm.SiteStatusRegistered, su, nil, nil, nil)
	common.TestBuildTenantSite(t, dbSession, stn, ss, su)

	// Case: User with both Provider/Tenant role, has access to Site owned by another org
	ipOrg2 := "test-provider-org-2"
	ipRoles2 := []string{authz.ProviderAdminRole}
	ipu2 := testSiteBuildUser(t, dbSession, uuid.NewString(), ipOrg2, ipRoles2)
	ip2 := testSiteBuildInfrastructureProvider(t, dbSession, "test-provider-2", ipOrg2, ipu2)
	st2 := testSiteBuildSite(t, dbSession, ip2, "test-site-2", cdbm.SiteStatusRegistered, ipu2, nil, nil, nil)

	mOrg := "test-mixed-org"
	mixedRole := []string{authz.ProviderAdminRole, authz.TenantAdminRole}
	mu := testSiteBuildUser(t, dbSession, uuid.NewString(), mOrg, mixedRole)
	_ = testSiteBuildInfrastructureProvider(t, dbSession, "test-mixed-provider", mOrg, mu)
	mtn := testSiteBuildTenant(t, dbSession, "test-mixed-tenant", mOrg, mu)

	testSiteBuildAllocation(t, dbSession, st2, mtn, "test-mixed-allocation", ipu2)
	common.TestBuildTenantSite(t, dbSession, mtn, st2, ipu2)

	// Setup echo server/context
	e := echo.New()
	req := httptest.NewRequest(http.MethodGet, "/", nil)
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	type args struct {
		org              string
		site             *cdbm.Site
		ts               *cdbm.TenantSite
		query            url.Values
		user             *cdbm.User
		isServiceAccount bool
	}

	tests := []struct {
		name                  string
		fields                fields
		args                  args
		wantRespCode          int
		wantErr               bool
		verifyIncludeRelation bool
		verifyChildSpanner    bool
	}{
		{
			name: "test Site retrieval by Provider Admin success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				site: st,
				user: ipu,
			},
			wantRespCode: http.StatusOK,
			wantErr:      false,
		},
		{
			name: "test Site retrieval by Provider Viewer success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				site: st,
				user: ipuv,
			},
			wantRespCode: http.StatusOK,
			wantErr:      false,
		},
		{
			name: "test Site retrieval by Infrastructure Provider, failure for invalid Site ID",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				site: &cdbm.Site{ID: uuid.New()},
				user: ipu,
			},
			wantRespCode: http.StatusNotFound,
			wantErr:      false,
		},
		{
			name: "test Site retrieval by Tenant with Allocation success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg1,
				site: st,
				ts:   ts,
				user: tnu1,
			},
			wantRespCode: http.StatusOK,
			wantErr:      false,
		},
		{
			name: "test Site retrieval by Tenant with Allocation and including relation success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:   tnOrg1,
				site:  st,
				ts:    ts,
				query: url.Values{"includeRelation": []string{cdbm.InfrastructureProviderRelationName}},
				user:  tnu1,
			},
			wantRespCode:          http.StatusOK,
			wantErr:               false,
			verifyIncludeRelation: true,
		}, {
			name: "test Site retrieval by Tenant with no targetedInstanceCreation capability and no Allocation",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg2,
				site: st,
				user: tnu2,
			},
			wantRespCode:       http.StatusForbidden,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Site retrieval by Tenant with targetedInstanceCreation capability but no Allocation",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg3,
				site: st,
				user: tnu3,
			},
			wantRespCode:       http.StatusOK,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Site retrieval by Tenant with Allocation, failure for invalid Site ID",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg1,
				site: &cdbm.Site{ID: uuid.New()},
				user: tnu1,
			},
			wantRespCode: http.StatusNotFound,
			wantErr:      false,
		},
		{
			name: "test Site retrieval failure when user does not have required role",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  vOrg1,
				site: &cdbm.Site{ID: uuid.New()},
				user: vu1,
			},
			wantRespCode: http.StatusForbidden,
			wantErr:      false,
		},
		{
			name: "test Site retrieval failure when org does not have any association with Site",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  vOrg2,
				site: st,
				user: vu2,
			},
			wantRespCode: http.StatusForbidden,
			wantErr:      false,
		},
		{
			name: "test Site retrieval success by Service Account",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:              sOrg,
				site:             ss,
				user:             su,
				isServiceAccount: true,
			},
			wantRespCode: http.StatusOK,
			wantErr:      false,
		},
		{
			name: "test Site retrieval by user with both Provider/Tenant role, and has Allocation for Site owned by another org",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  mOrg,
				site: st2,
				user: mu,
			},
			wantRespCode: http.StatusOK,
			wantErr:      false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gsh := GetSiteHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/site/%v?%s", tt.args.org, tt.args.site.ID.String(), tt.args.query.Encode())
			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.org, tt.args.site.ID.String())
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := gsh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("GetSiteHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.wantRespCode != rec.Code {
				t.Errorf("GetSiteHandler.Handle() response = %v", rec.Body.String())
			}

			require.Equal(t, tt.wantRespCode, rec.Code)
			if tt.wantRespCode != http.StatusOK {
				return
			}

			rst := &model.APISite{}
			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, rst.ID, tt.args.site.ID.String())
			assert.Equal(t, rst.Name, tt.args.site.Name)
			assert.Equal(t, *rst.Description, *st.Description)

			if tt.args.ts != nil {
				assert.Equal(t, *rst.IsSerialConsoleSSHKeysEnabled, tt.args.ts.EnableSerialConsole)
			}

			if tt.verifyIncludeRelation {
				require.NotNil(t, rst.InfrastructureProvider)
				assert.Equal(t, rst.InfrastructureProvider.Org, ip.Org)
				if ip.OrgDisplayName != nil {
					assert.Equal(t, *rst.InfrastructureProvider.OrgDisplayName, *ip.OrgDisplayName)
				}
			} else {
				assert.Nil(t, rst.InfrastructureProvider)
			}

			if tt.args.site.Status == cdbm.SiteStatusRegistered {
				assert.True(t, rst.IsOnline)
			} else {
				assert.False(t, rst.IsOnline)
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetAllSiteHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	testSiteSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrg2 := "test-provider-org-2"
	ipRoles := []string{authz.ProviderAdminRole}
	ipvRoles := []string{authz.ProviderViewerRole}

	ipu1 := testSiteBuildUser(t, dbSession, "test123", ipOrg, ipRoles)
	ipuv := testSiteBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipvRoles)
	ip1 := testSiteBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider 1", ipOrg, ipu1)

	ipu2 := testSiteBuildUser(t, dbSession, "test1234", ipOrg2, ipRoles)
	ip2 := testSiteBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider 2", ipOrg2, ipu2)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testSiteBuildUser(t, dbSession, "test456", tnOrg, tnRoles)
	tn := testSiteBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	sOrg := "test-service-org"
	sRoles := []string{authz.ProviderAdminRole, authz.TenantAdminRole}
	su := testSiteBuildUser(t, dbSession, "test-service-user", sOrg, sRoles)
	sip := testSiteBuildInfrastructureProvider(t, dbSession, "Test Service Provider", sOrg, su)
	stn := testSiteBuildTenant(t, dbSession, "Test Service Tenant", sOrg, su)

	totalCount := 50

	location1 := &cdbm.SiteLocation{
		City:    "San Jose",
		State:   "CA",
		Country: "USA",
	}
	location2 := &cdbm.SiteLocation{
		City:    "Seattle",
		State:   "WA",
		Country: "USA",
	}
	contact1 := &cdbm.SiteContact{
		Email: "alan@nvidia.com",
	}
	contact2 := &cdbm.SiteContact{
		Email: "john@nvidia.com",
	}

	sts := []cdbm.Site{}
	for i := 0; i < totalCount; i++ {
		var st *cdbm.Site
		if i%2 == 0 {
			st = testSiteBuildSite(t, dbSession, ip1, fmt.Sprintf("test-site-%02d", i), cdbm.SiteStatusRegistered, ipu1, location1, contact1, nil)
			testSiteBuildAllocation(t, dbSession, st, tn, fmt.Sprintf("test-allocation-%02d", i), ipu1)
			common.TestBuildTenantSite(t, dbSession, tn, st, ipu1)
		} else {
			st = testSiteBuildSite(t, dbSession, ip1, fmt.Sprintf("test-site-%02d search", i), cdbm.SiteStatusRegistered, ipu1, location2, contact2, nil)
		}

		common.TestBuildStatusDetail(t, dbSession, st.ID.String(), cdbm.SiteStatusPending, cutil.GetPtr("request received, pending processing"))
		common.TestBuildStatusDetail(t, dbSession, st.ID.String(), cdbm.SiteStatusPending, cutil.GetPtr("Site is now ready for use"))
		sts = append(sts, *st)
	}

	// Second Site
	stdemo1 := testSiteBuildSite(t, dbSession, ip2, "pdx-demo1", cdbm.SiteStatusRegistered, ipu2, nil, nil, nil)
	common.TestBuildStatusDetail(t, dbSession, stdemo1.ID.String(), cdbm.SiteStatusPending, cutil.GetPtr("request received, pending processing"))
	common.TestBuildStatusDetail(t, dbSession, stdemo1.ID.String(), cdbm.SiteStatusPending, cutil.GetPtr("Site is now ready for use"))

	stdemo1m1 := testSiteBuildMachine(t, dbSession, ip2.ID, stdemo1.ID, cdbm.MachineStatusReady)
	stdemo1m2 := testSiteBuildMachine(t, dbSession, ip2.ID, stdemo1.ID, cdbm.MachineStatusReady)
	_ = testSiteBuildMachine(t, dbSession, ip2.ID, stdemo1.ID, cdbm.MachineStatusError)
	_ = testSiteBuildMachine(t, dbSession, ip2.ID, stdemo1.ID, cdbm.MachineStatusError)

	// GPU capabilities for stdemo1 machines, used by the includeGpuStats case.
	// stdemo1 -> NVIDIA H100: 16 GPUs across 2 machines.
	common.TestBuildMachineCapability(t, dbSession, &stdemo1m1.ID, nil, cdbm.MachineCapabilityTypeGPU, "NVIDIA H100", nil, nil, nil, cutil.GetPtr(8), nil, nil)
	common.TestBuildMachineCapability(t, dbSession, &stdemo1m2.ID, nil, cdbm.MachineCapabilityTypeGPU, "NVIDIA H100", nil, nil, nil, cutil.GetPtr(8), nil, nil)

	stdemo2 := testSiteBuildSite(t, dbSession, ip2, "pdx-dev3", cdbm.SiteStatusRegistered, ipu2, nil, nil, nil)
	common.TestBuildStatusDetail(t, dbSession, stdemo2.ID.String(), cdbm.SiteStatusPending, cutil.GetPtr("request received, pending processing"))
	common.TestBuildStatusDetail(t, dbSession, stdemo2.ID.String(), cdbm.SiteStatusPending, cutil.GetPtr("Site is now ready for use"))

	_ = testSiteBuildMachine(t, dbSession, ip2.ID, stdemo2.ID, cdbm.MachineStatusReady)
	_ = testSiteBuildMachine(t, dbSession, ip2.ID, stdemo2.ID, cdbm.MachineStatusReady)
	_ = testSiteBuildMachine(t, dbSession, ip2.ID, stdemo2.ID, cdbm.MachineStatusError)
	_ = testSiteBuildMachine(t, dbSession, ip2.ID, stdemo2.ID, cdbm.MachineStatusError)

	// Build Site for Service Provider
	ss := testSiteBuildSite(t, dbSession, sip, "test-service-site", cdbm.SiteStatusRegistered, su, nil, nil, nil)
	common.TestBuildTenantSite(t, dbSession, stn, ss, su)
	common.TestBuildStatusDetail(t, dbSession, ss.ID.String(), cdbm.SiteStatusPending, cutil.GetPtr("request received, pending processing"))
	common.TestBuildStatusDetail(t, dbSession, ss.ID.String(), cdbm.SiteStatusPending, cutil.GetPtr("Site is now ready for use"))

	// Update Sites with feature flags
	stDAO := cdbm.NewSiteDAO(dbSession)
	stDAO.Update(ctx, nil, cdbm.SiteUpdateInput{Config: &cdbm.SiteConfigUpdateInput{NativeNetworking: cutil.GetPtr(true)}, SiteID: sts[0].ID})
	stDAO.Update(ctx, nil, cdbm.SiteUpdateInput{Config: &cdbm.SiteConfigUpdateInput{NativeNetworking: cutil.GetPtr(true)}, SiteID: sts[1].ID})

	stDAO.Update(ctx, nil, cdbm.SiteUpdateInput{Config: &cdbm.SiteConfigUpdateInput{NetworkSecurityGroup: cutil.GetPtr(true)}, SiteID: sts[2].ID})
	stDAO.Update(ctx, nil, cdbm.SiteUpdateInput{Config: &cdbm.SiteConfigUpdateInput{NetworkSecurityGroup: cutil.GetPtr(true)}, SiteID: sts[3].ID})

	stDAO.Update(ctx, nil, cdbm.SiteUpdateInput{Config: &cdbm.SiteConfigUpdateInput{NativeNetworking: cutil.GetPtr(true), NetworkSecurityGroup: cutil.GetPtr(true), NVLinkPartition: cutil.GetPtr(true), Flow: cutil.GetPtr(true)}, SiteID: sts[4].ID})
	stDAO.Update(ctx, nil, cdbm.SiteUpdateInput{Config: &cdbm.SiteConfigUpdateInput{NativeNetworking: cutil.GetPtr(true), NetworkSecurityGroup: cutil.GetPtr(true), NVLinkPartition: cutil.GetPtr(true), Flow: cutil.GetPtr(true)}, SiteID: sts[5].ID})

	// Setup echo server/context
	e := echo.New()

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	type args struct {
		org              string
		query            url.Values
		user             *cdbm.User
		requestIP        *cdbm.InfrastructureProvider
		isServiceAccount bool
		includeRelation  bool
	}

	tests := []struct {
		name                   string
		fields                 fields
		args                   args
		wantCount              int
		wantTotalCount         int
		wantRespCode           int
		wantFirstEntry         *cdbm.Site
		wantMachineStats       map[string]int
		wantGpuStatsRequested  bool
		wantGpuStatsForSite    string
		wantGpuStats           map[string]int
		verifyTenantAttributes bool
		verifyChildSpanner     bool
	}{
		{
			name: "get all Sites by Provider admin success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				user: ipu1,
			},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     totalCount,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by Provider admin with machine stats success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg2,
				query: url.Values{
					"includeMachineStats": []string{"True"},
				},
				user: ipu2,
			},
			wantCount: 2,
			wantMachineStats: map[string]int{
				cdbm.MachineStatusReady: 2,
				cdbm.MachineStatusError: 2,
			},
			wantTotalCount:     2,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by Provider admin with gpu stats success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg2,
				query: url.Values{
					"includeGpuStats": []string{"True"},
				},
				user: ipu2,
			},
			wantCount:             2,
			wantTotalCount:        2,
			wantRespCode:          http.StatusOK,
			wantGpuStatsRequested: true,
			wantGpuStatsForSite:   "pdx-demo1",
			wantGpuStats: map[string]int{
				"NVIDIA H100": 16,
			},
		},
		{
			name: "includeGpuStats denied for tenant user",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"includeGpuStats": []string{"True"},
				},
				user: tnu,
			},
			wantRespCode: http.StatusForbidden,
		},
		{
			name: "get all Sites by Provider admin with native networking enabled success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"isNativeNetworkingEnabled": []string{"True"},
				},
				user: ipu1,
			},
			wantCount:          4,
			wantTotalCount:     4,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by Provider admin with NVLink partition enabled success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"isNVLinkPartitionEnabled": []string{"True"},
				},
				user: ipu1,
			},
			wantCount:          2,
			wantTotalCount:     2,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by Provider admin with rack level administration enabled success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"isFlowEnabled": []string{"True"},
				},
				user: ipu1,
			},
			wantCount:          2,
			wantTotalCount:     2,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by Provider admin with network security group enabled success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"isNetworkSecurityGroupEnabled": []string{"True"},
				},
				user: ipu1,
			},
			wantCount:          4,
			wantTotalCount:     4,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by Provider admin with NSG enabled success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"isNetworkSecurityGroupEnabled": []string{"True"},
				},
				user: ipu1,
			},
			wantCount:          4,
			wantTotalCount:     4,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by Provider admin with multiple site feature flags - success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"isNetworkSecurityGroupEnabled": []string{"True"},
					"isNativeNetworkingEnabled":     []string{"True"},
				},
				user: ipu1,
			},
			//  We only expect 2 because only two sites have BOTH flags enabled.
			wantCount:          2,
			wantTotalCount:     2,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by Provider viewer success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				user: ipuv,
			},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     totalCount,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all Sites by Infrastructure Provider with pagination success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"pageNumber": []string{"2"},
					"pageSize":   []string{"5"},
					"orderBy":    []string{"NAME_ASC"},
				},
				user: ipu1,
			},
			wantCount:      5,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantFirstEntry: &sts[5], // Test Site 14
		},
		{
			name: "get all Sites by Infrastructure Provider with pagination success - order by description",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"pageNumber": []string{"2"},
					"pageSize":   []string{"5"},
					"orderBy":    []string{"DESCRIPTION_ASC"},
				},
				user: ipu1,
			},
			wantCount:      5,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all Sites by Infrastructure Provider with pagination failure, invalid page size",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"pageNumber": []string{"1"},
					"pageSize":   []string{"200"},
					"orderBy":    []string{"NAME_ASC"},
				},
				user: ipu1,
			},
			wantRespCode: http.StatusBadRequest,
		},
		{
			name: "get all Sites by Tenant with Allocation success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"tenantId": []string{tn.ID.String()},
				},
				user: tnu,
			},
			verifyTenantAttributes: true,
			wantCount:              20,
			wantTotalCount:         totalCount / 2,
			wantRespCode:           http.StatusOK,
		},
		{
			name: "get all Sites by Service Account success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: sOrg,
				query: url.Values{
					"includeMachineStats": []string{"True"},
				},
				user:             su,
				isServiceAccount: true,
			},
			wantCount:      1,
			wantTotalCount: 1,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all Sites by Tenant with machine status, failure",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"tenantId":            []string{tn.ID.String()},
					"includeMachineStats": []string{"True"},
				},
				user: tnu,
			},
			verifyTenantAttributes: true,
			wantRespCode:           http.StatusForbidden,
		},
		{
			name: "get all Sites by Tenant with Allocation and including relation success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"includeRelation": []string{cdbm.InfrastructureProviderRelationName},
				},
				user:            tnu,
				requestIP:       ip1,
				includeRelation: true,
			},
			verifyTenantAttributes: true,
			wantCount:              20,
			wantTotalCount:         totalCount / 2,
			wantRespCode:           http.StatusOK,
		},
		{
			name: "get all Sites by name query search success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"query": []string{"t-sit"},
				},
				user: ipu1,
			},
			wantCount:      20,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all Sites by substring name query search success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"query": []string{"search"},
				},
				user: ipu1,
			},
			wantCount:      20,
			wantTotalCount: totalCount / 2,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all Sites by custom substring name query search success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg2,
				query: url.Values{
					"query": []string{"pd"},
				},
				user: ipu2,
			},
			wantCount:      2,
			wantTotalCount: 2,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all Sites by status query success, no results found for invalid status",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"query": []string{"ready"},
				},
				user: tnu,
			},
			verifyTenantAttributes: true,
			wantCount:              0,
			wantTotalCount:         0,
			wantRespCode:           http.StatusOK,
		},
		{
			name: "get all Sites by status query search success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"query": []string{"registered"},
				},
				user: tnu,
			},
			verifyTenantAttributes: true,
			wantCount:              20,
			wantTotalCount:         totalCount / 2,
			wantRespCode:           http.StatusOK,
		},
		{
			name: "get all Sites by Registered status query success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"status": []string{cdbm.SiteStatusRegistered},
				},
				user: tnu,
			},
			verifyTenantAttributes: true,
			wantCount:              20,
			wantTotalCount:         totalCount / 2,
			wantRespCode:           http.StatusOK,
		},
		{
			name: "get all Sites by multiple status query success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"status": []string{cdbm.SiteStatusRegistered, cdbm.SiteStatusPending},
				},
				user: tnu,
			},
			verifyTenantAttributes: true,
			wantCount:              20,
			wantTotalCount:         totalCount / 2,
			wantRespCode:           http.StatusOK,
		},
		{
			name: "get all Sites by BadStatus status query success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"status": []string{"BadStatus"},
				},
				user: tnu,
			},
			verifyTenantAttributes: true,
			wantCount:              0,
			wantTotalCount:         0,
			wantRespCode:           http.StatusBadRequest,
		},
		{
			name: "get all Sites order by location",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"orderBy": []string{"LOCATION_ASC"},
				},
				user: ipu1,
			},
			wantCount:      20,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantFirstEntry: &sts[0],
		},
		{
			name: "get all Sites order by contact",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"orderBy": []string{"CONTACT_ASC"},
				},
				user: ipu1,
			},
			wantCount:      20,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantFirstEntry: &sts[0],
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gash := GetAllSiteHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/site?%s", tt.args.org, tt.args.query.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := gash.Handle(ec)
			require.NoError(t, err)
			require.Equal(t, tt.wantRespCode, rec.Code, rec.Body.String())

			if tt.wantRespCode != http.StatusOK {
				return
			}

			resp := []model.APISite{}

			serr := json.Unmarshal(rec.Body.Bytes(), &resp)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, tt.wantCount, len(resp))

			if tt.wantFirstEntry != nil {
				assert.Equal(t, tt.wantFirstEntry.Name, resp[0].Name)
			}

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err = json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			assert.Equal(t, tt.wantTotalCount, pr.Total)

			for _, apist := range resp {
				assert.Equal(t, 2, len(apist.StatusHistory))

				if tt.wantMachineStats != nil {
					statsLen := len(tt.wantMachineStats)
					assert.NotNil(t, apist.MachineStats)
					assert.GreaterOrEqual(t, apist.MachineStats.Total, 1)
					assert.GreaterOrEqual(t, len(apist.MachineStats.TotalByStatus), statsLen)

					for key := range tt.wantMachineStats {
						assert.Equal(t, tt.wantMachineStats[key], apist.MachineStats.TotalByStatus[key])
						assert.Equal(t, tt.wantMachineStats[key], apist.MachineStats.TotalByStatusAndHealth[key]["healthy"])
					}

					assert.NotNil(t, apist.MachineStats.TotalByAllocation)
				}

				if tt.wantGpuStatsRequested {
					// Field is always populated (non-nil) when requested; sites
					// with no GPUs get an empty slice.
					require.NotNil(t, apist.GpuStats)

					if apist.Name == tt.wantGpuStatsForSite {
						gpuByName := map[string]int{}
						for _, g := range *apist.GpuStats {
							gpuByName[g.Name] = g.GPUs
						}
						for name, gpus := range tt.wantGpuStats {
							assert.Equal(t, gpus, gpuByName[name])
						}
					}
				}

				if tt.args.includeRelation {
					require.NotNil(t, apist.InfrastructureProvider)
					assert.Equal(t, tt.args.requestIP.Org, apist.InfrastructureProvider.Org)
					if tt.args.requestIP != nil && tt.args.requestIP.OrgDisplayName != nil {
						assert.Equal(t, *tt.args.requestIP.OrgDisplayName, *apist.InfrastructureProvider.OrgDisplayName)
					}
				} else {
					assert.Nil(t, apist.InfrastructureProvider)
				}

				if tt.verifyTenantAttributes {
					assert.NotNil(t, apist.IsSerialConsoleSSHKeysEnabled)
				} else if !tt.args.isServiceAccount {
					assert.Nil(t, apist.IsSerialConsoleSSHKeysEnabled)
				}
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

// TestGetAllSiteHandler_NullConfig reproduces NVBug 6072342:
//
//	~ cli site list
//	0 items
//	[]
//
//	~ cli site create --name "reno-qa4"
//	Error: API error 409: A Site with specified name already exists for Infrastructure Provider
//	Details: {"id":"bc4f6b79-f6b8-46f8-9921-0978789332907"}
//
// Root cause: GetAllSiteHandler unconditionally sets filter.Config to an
// empty SiteConfigFilterInput, adding WHERE st.config @> '{}'::jsonb to
// every list query.  NULL @> '{}' evaluates to NULL (falsy in SQL), so
// sites with a NULL config column are silently excluded.  The create
// handler's duplicate check does NOT apply this filter, so it finds the
// site and returns 409 -- contradiction.
func TestGetAllSiteHandler_NullConfig(t *testing.T) {
	ctx := context.Background()
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	testSiteSetupSchema(t, dbSession)

	org := "reno-qa4-org"
	roles := []string{authz.ProviderAdminRole}
	user := testSiteBuildUser(t, dbSession, "qa4-admin", org, roles)
	ip := testSiteBuildInfrastructureProvider(t, dbSession, "QA4 Provider", org, user)

	// Create a site named "reno-qa4" (same as the bug report).
	site := testSiteBuildSite(t, dbSession, ip, "reno-qa4", cdbm.SiteStatusRegistered, user, nil, nil, nil)
	require.NotNil(t, site)
	common.TestBuildStatusDetail(t, dbSession, site.ID.String(), cdbm.SiteStatusPending, cutil.GetPtr("pending"))
	common.TestBuildStatusDetail(t, dbSession, site.ID.String(), cdbm.SiteStatusRegistered, cutil.GetPtr("registered"))

	// Simulate the DB state that causes the bug: config column is NULL.
	// This can happen when sites predate the config migration or when the
	// migration copied a NULL capabilities value.
	_, err := dbSession.DB.NewUpdate().
		Model((*cdbm.Site)(nil)).
		Set("config = NULL").
		Where("id = ?", site.ID).
		Exec(ctx)
	require.NoError(t, err)

	e := echo.New()
	cfg := common.GetTestConfig()
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// ---- Step 1: cli site list ----
	gash := GetAllSiteHandler{
		dbSession: dbSession,
		tc:        &tmocks.Client{},
		cfg:       cfg,
	}

	listPath := fmt.Sprintf("/v2/org/%s/nico/site", org)
	listReq := httptest.NewRequest(http.MethodGet, listPath, nil)
	listReq.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	listRec := httptest.NewRecorder()
	listCtx := e.NewContext(listReq, listRec)
	listCtx.SetParamNames("orgName")
	listCtx.SetParamValues(org)
	listCtx.Set("user", user)
	listCtx.SetRequest(listReq.WithContext(context.WithValue(ctx, otelecho.TracerKey, tracer)))

	err = gash.Handle(listCtx)
	require.NoError(t, err)
	require.Equal(t, http.StatusOK, listRec.Code)

	ph := listRec.Header().Get(pagination.ResponseHeaderName)
	require.NotEmpty(t, ph)
	pr := &pagination.PageResponse{}
	require.NoError(t, json.Unmarshal([]byte(ph), pr))

	var listResp []model.APISite
	require.NoError(t, json.Unmarshal(listRec.Body.Bytes(), &listResp))

	t.Logf("--- cli site list ---")
	t.Logf("%d items", pr.Total)
	t.Logf("%s", listRec.Body.String())

	// ---- Step 2: cli site create --name "reno-qa4" ----
	createBody := `{"name":"reno-qa4"}`
	createPath := fmt.Sprintf("/v2/org/%s/nico/site", org)
	createReq := httptest.NewRequest(http.MethodPost, createPath, strings.NewReader(createBody))
	createReq.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	createRec := httptest.NewRecorder()
	createCtx := e.NewContext(createReq, createRec)
	createCtx.SetParamNames("orgName")
	createCtx.SetParamValues(org)
	createCtx.Set("user", user)
	createCtx.SetRequest(createReq.WithContext(context.WithValue(ctx, otelecho.TracerKey, tracer)))

	csh := CreateSiteHandler{
		dbSession:  dbSession,
		tc:         &tmocks.Client{},
		cfg:        cfg,
		tracerSpan: sutil.NewTracerSpan(),
	}
	err = csh.Handle(createCtx)
	require.NoError(t, err)

	t.Logf("--- cli site create --name reno-qa4 ---")
	t.Logf("HTTP %d: %s", createRec.Code, createRec.Body.String())

	// The create handler can find the site (no config filter) and returns
	// 409.  This proves the site exists in the DB.
	assert.Equal(t, http.StatusConflict, createRec.Code,
		"create must return 409 proving the site exists")

	// The list handler must also find the site.  Before the fix it
	// returned 0 items due to the unconditional config JSONB filter.
	assert.Equal(t, 1, len(listResp),
		"site list must return the site even when config is NULL")
	assert.Equal(t, 1, pr.Total,
		"pagination total must include the site with NULL config")
}

func TestDeleteSiteHandler_Handle(t *testing.T) {
	ctx := context.Background()

	tcsm := &testCsm{}
	tcsm.setup(t)
	defer tcsm.teardown()

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	testSiteSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testSiteBuildUser(t, dbSession, "test123", ipOrg, ipRoles)
	ip := testSiteBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testSiteBuildUser(t, dbSession, "test456", tnOrg, tnRoles)
	assert.NotNil(t, tnu)

	tn := testSiteBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)
	assert.NotNil(t, tn)

	st := testSiteBuildSite(t, dbSession, ip, "Test Site", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)

	st3 := testSiteBuildSite(t, dbSession, ip, "Test Site 3", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)

	st4 := testSiteBuildSite(t, dbSession, ip, "Test Site 4", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)
	testSiteBuildAllocation(t, dbSession, st4, tn, "Test Allocation", ipu)

	st5 := testSiteBuildSite(t, dbSession, ip, "Test Site 5", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)
	common.TestBuildIPBlock(t, dbSession, "Test IP Block", st5, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "10.180.124.192", 28, cdbm.IPBlockProtocolVersionV4, ipu)

	st6 := testSiteBuildSite(t, dbSession, ip, "Test Site 6", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)
	common.TestBuildInstanceType(t, dbSession, "Test Instance Type", cutil.GetPtr(uuid.New()), st6, map[string]string{
		"name":        "Test Instance Type",
		"description": "Test Instance Type Description",
	}, ipu)

	st7 := testSiteBuildSite(t, dbSession, ip, "Test Site 7", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)

	st8 := testSiteBuildSite(t, dbSession, ip, "Test Site 8", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)

	st9 := testSiteBuildSite(t, dbSession, ip, "Test Site 9", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)

	cfg := common.GetTestConfig()

	cfg.SetSiteManagerEnabled(true)
	cfg.SetSiteManagerEndpoint(tcsm.getURL())

	tc := &tmocks.Client{}
	tc2 := &tmocks.Client{}

	gmockctrl := gomock.NewController(t)
	tosc := tosv1mock.NewMockOperatorServiceClient(gmockctrl)
	tosc.EXPECT().DeleteNamespace(gomock.Any(), gomock.Any()).Return(&tOperatorv1.DeleteNamespaceResponse{}, nil).AnyTimes()

	tc.Mock.On("OperatorService").Return(tosc)
	tc2.Mock.On("OperatorService").Return(tosc)

	// Init Temporal error response
	gmockctrl1 := gomock.NewController(t)
	tosc1 := tosv1mock.NewMockOperatorServiceClient(gmockctrl1)
	tosc1.EXPECT().DeleteNamespace(gomock.Any(), gomock.Any()).Return(nil, fmt.Errorf("Namespace %s is not found", st8.ID.String())).AnyTimes()

	tc.Mock.On("OperatorService").Return(tosc1)
	tc2.Mock.On("OperatorService").Return(tosc1)

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID, bool) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID"), false).Return(wrun, nil)

	// This client is used to test call to workflow with purgeMachines flag set to true
	tc2.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID, bool) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID"), true).Return(wrun, nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		org   string
		id    string
		query url.Values
		user  *cdbm.User
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		remainSiteCnt      int
		siteMgrErr         bool
		siteMgrErrCode     *int
		siteMgrDisabled    bool
		verifyChildSpanner bool
	}{
		{
			name: "ok Site deletion API endpoint",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				id:   st.ID.String(),
				user: ipu,
			},
			wantErr:            false,
			remainSiteCnt:      7,
			verifyChildSpanner: true,
		},
		{
			name: "error Site deletion when Allocation is present",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				id:   st4.ID.String(),
				user: ipu,
			},
			wantErr:       true,
			remainSiteCnt: 6,
		},
		{
			name: "error Site deletion API endpoint, sitemgr error",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				id:   st3.ID.String(),
				user: ipu,
			},
			wantErr:       true,
			remainSiteCnt: 5,
			siteMgrErr:    true,
		},
		{
			name: "ok Site deletion API endpoint, sitemgr site not found error",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				id:   st3.ID.String(),
				user: ipu,
			},
			wantErr:        false,
			remainSiteCnt:  6,
			siteMgrErr:     true,
			siteMgrErrCode: cutil.GetPtr(http.StatusNotFound),
		},
		{
			name: "ok Site deletion API endpoint, sitemgr disabled",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				id:   st7.ID.String(),
				user: ipu,
			},
			wantErr:         false,
			remainSiteCnt:   5,
			siteMgrDisabled: true,
		},
		{
			name: "ok Site deletion API endpoint, Temporal namespace was not found",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:  ipOrg,
				id:   st8.ID.String(),
				user: ipu,
			},
			wantErr:         false,
			remainSiteCnt:   4,
			siteMgrDisabled: true,
		},
		{
			name: "ok Site deletion API endpoint with query",
			fields: fields{
				dbSession: dbSession,
				tc:        tc2,
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				id:  st9.ID.String(),
				query: url.Values{
					"purgeMachines": []string{"true"},
				},
				user: ipu,
			},
			wantErr:       false,
			remainSiteCnt: 3,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tcsm.forceErr = tt.siteMgrErr
			if tt.siteMgrErrCode != nil {
				tcsm.errCode = *tt.siteMgrErrCode
			}
			// Setup echo server/context
			e := echo.New()

			path := fmt.Sprintf("/v2/org/%v/nico/site/%v?%v", tt.args.org, tt.args.id, tt.args.query.Encode())

			req := httptest.NewRequest(http.MethodDelete, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.org, tt.args.id)
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			dsh := DeleteSiteHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}
			if tt.siteMgrDisabled {
				tt.fields.cfg.SetSiteManagerEnabled(false)
				tt.fields.cfg.SetSiteManagerEndpoint("")
			}

			err := dsh.Handle(ec)
			assert.Nil(t, err)

			if !tt.wantErr {
				require.Equal(t, http.StatusAccepted, rec.Code)
			}

			if !tt.wantErr {
				stDAO := cdbm.NewSiteDAO(dbSession)
				ipsts, _, terr := stDAO.GetAll(context.Background(), nil, cdbm.SiteFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}}, paginator.PageInput{}, nil)
				assert.Nil(t, terr)
				assert.Equal(t, tt.remainSiteCnt, len(ipsts))
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNewCreateSiteHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		tnc       temporalClient.NamespaceClient
		cfg       *config.Config
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	tnc := &tmocks.NamespaceClient{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want CreateSiteHandler
	}{
		{
			name: "test CreateSiteHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				tnc:       tnc,
				cfg:       cfg,
			},
			want: CreateSiteHandler{
				dbSession:  dbSession,
				tc:         tc,
				tnc:        tnc,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewCreateSiteHandler(tt.args.dbSession, tt.args.tc, tt.args.tnc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewCreateSiteHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNewUpdateSiteHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want UpdateSiteHandler
	}{
		{
			name: "test UpdateSiteHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: UpdateSiteHandler{
				dbSession:  dbSession,
				tc:         tc,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewUpdateSiteHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewUpdateSiteHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNewGetSiteHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want GetSiteHandler
	}{
		{
			name: "test GetSiteHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetSiteHandler{
				dbSession:  dbSession,
				tc:         tc,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetSiteHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewGetSiteHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNewGetAllSiteHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want GetAllSiteHandler
	}{
		{
			name: "test GetAllSiteHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetAllSiteHandler{
				dbSession:  dbSession,
				tc:         tc,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetAllSiteHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewGetAllSiteHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNewDeleteSiteHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want DeleteSiteHandler
	}{
		{
			name: "test DeleteSiteHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: DeleteSiteHandler{
				dbSession:  dbSession,
				tc:         tc,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewDeleteSiteHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewDeleteSiteHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestSiteHandler_GetStatusDetails(t *testing.T) {
	ctx := context.Background()
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testSiteSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipvRoles := []string{authz.ProviderViewerRole}

	ipu := testSiteBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ipuv := testSiteBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipvRoles)
	ip := testSiteBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", ipOrg, ipu)
	st := testSiteBuildSite(t, dbSession, ip, "Test Site", cdbm.SiteStatusRegistered, ipu, nil, nil, nil)

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrg3 := "test-tenant-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu1 := testSiteBuildUser(t, dbSession, uuid.NewString(), tnOrg1, tnRoles)
	assert.NotNil(t, tnu1)

	tnu2 := testSiteBuildUser(t, dbSession, uuid.NewString(), tnOrg2, tnRoles)
	assert.NotNil(t, tnu2)

	tnu3 := testSiteBuildUser(t, dbSession, uuid.NewString(), tnOrg3, tnRoles)
	assert.NotNil(t, tnu3)

	tn1 := testSiteBuildTenant(t, dbSession, "Test Tenant 1", tnOrg1, tnu1)
	assert.NotNil(t, tn1)

	tn2 := testSiteBuildTenant(t, dbSession, "Test Tenant 2", tnOrg2, tnu2)
	assert.NotNil(t, tn2)

	tn3 := testSiteBuildTenant(t, dbSession, "Test Tenant 3", tnOrg3, tnu3)
	assert.NotNil(t, tn3)

	// Tenant 3 is privileged via Ready TenantAccount.config
	ta3 := common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip, &tn3.ID, tnOrg3, cdbm.TenantAccountStatusReady, tnu3)
	assert.NotNil(t, ta3)

	// Tenant 1 has an Allocation
	testSiteBuildAllocation(t, dbSession, st, tn1, "Test Allocation", ipu)
	common.TestBuildTenantSite(t, dbSession, tn1, st, ipu)

	vOrg1 := "test-visitor-org-1"
	vu1 := testSiteBuildUser(t, dbSession, uuid.NewString(), vOrg1, []string{"RANDDOM_ROLE"})

	vOrg2 := "test-visitor-org-2"
	vu2 := testSiteBuildUser(t, dbSession, uuid.NewString(), vOrg2, ipRoles)

	mOrg := "test-mixed-org"
	mOrg2 := "test-mixed-org-2"
	mixedRole := []string{authz.ProviderAdminRole, authz.TenantAdminRole}

	mu := testSiteBuildUser(t, dbSession, uuid.NewString(), mOrg, mixedRole)
	mu2 := testSiteBuildUser(t, dbSession, uuid.NewString(), mOrg2, mixedRole)

	mip := testSiteBuildInfrastructureProvider(t, dbSession, "test-mixed-provider", mOrg, mu)
	mtn := testSiteBuildTenant(t, dbSession, "test-mixed-tenant", mOrg, mu)
	mst := testSiteBuildSite(t, dbSession, mip, "test-mixed-site", cdbm.SiteStatusRegistered, mu, nil, nil, nil)
	testSiteBuildAllocation(t, dbSession, mst, mtn, "test-mixed-allocation", mu)
	common.TestBuildTenantSite(t, dbSession, mtn, mst, mu)

	// Case: User with both Provider/Tenant role, has access to Site owned by another org
	ipOrg2 := "test-provider-org-2"
	ipRoles2 := []string{authz.ProviderAdminRole}
	ipu2 := testSiteBuildUser(t, dbSession, uuid.NewString(), ipOrg2, ipRoles2)
	ip2 := testSiteBuildInfrastructureProvider(t, dbSession, "test-provider-2", ipOrg2, ipu2)
	st2 := testSiteBuildSite(t, dbSession, ip2, "test-site-", cdbm.SiteStatusRegistered, ipu2, nil, nil, nil)

	_ = testSiteBuildInfrastructureProvider(t, dbSession, "test-mixed-provider-2", mOrg2, mu2)
	mtn2 := testSiteBuildTenant(t, dbSession, "test-mixed-tenant-2", mOrg2, mu2)

	testSiteBuildAllocation(t, dbSession, st2, mtn2, "test-mixed-allocation-2", ipu2)
	common.TestBuildTenantSite(t, dbSession, mtn2, st2, mu2)

	// add status details objects
	totalCount := 30
	for i := 0; i < totalCount; i++ {
		if i%2 != 0 {
			testMachineBuildStatusDetail(t, dbSession, st.ID.String(), cdbm.MachineStatusInitializing, nil)
			testMachineBuildStatusDetail(t, dbSession, mst.ID.String(), cdbm.MachineStatusInitializing, nil)
			testMachineBuildStatusDetail(t, dbSession, st2.ID.String(), cdbm.MachineStatusInitializing, nil)
		} else {
			testMachineBuildStatusDetail(t, dbSession, st.ID.String(), cdbm.MachineStatusReady, nil)
			testMachineBuildStatusDetail(t, dbSession, mst.ID.String(), cdbm.MachineStatusInitializing, nil)
			testMachineBuildStatusDetail(t, dbSession, st2.ID.String(), cdbm.MachineStatusInitializing, nil)
		}
	}

	// init echo
	e := echo.New()
	req := httptest.NewRequest(http.MethodGet, "/", nil)
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

	// init handler
	handler := GetSiteStatusDetailsHandler{
		dbSession: dbSession,
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name      string
		reqSiteID string
		reqOrg    string
		reqUser   *cdbm.User
		query     url.Values
		respCode  int
	}{
		{
			name:      "success with Provider admin",
			reqSiteID: st.ID.String(),
			reqOrg:    ipOrg,
			reqUser:   ipu,
			respCode:  http.StatusOK,
		},
		{
			name:      "success with Provider viewer",
			reqSiteID: st.ID.String(),
			reqOrg:    ipOrg,
			reqUser:   ipuv,
			respCode:  http.StatusOK,
		},
		{
			name:      "failure retrieval by Infrastructure Provider invalid Site ID",
			reqSiteID: uuid.New().String(),
			reqOrg:    ipOrg,
			reqUser:   ipu,
			respCode:  http.StatusNotFound,
		},
		{
			name:      "success by Tenant with Allocation",
			reqSiteID: st.ID.String(),
			reqOrg:    tnOrg1,
			reqUser:   tnu1,
			respCode:  http.StatusOK,
		},
		{
			name:      "failure by Tenant with no Allocation",
			reqSiteID: st.ID.String(),
			reqOrg:    tnOrg2,
			reqUser:   tnu2,
			respCode:  http.StatusForbidden,
		},
		{
			name:      "failure for invalid Site ID by Tenant with Allocation",
			reqSiteID: uuid.New().String(),
			reqOrg:    tnOrg1,
			reqUser:   tnu1,
			respCode:  http.StatusNotFound,
		},
		{
			name:      "failure user does not have required role",
			reqSiteID: uuid.New().String(),
			reqOrg:    vOrg1,
			reqUser:   vu1,
			respCode:  http.StatusForbidden,
		},
		{
			name:      "failure org does not have Provider",
			reqSiteID: st.ID.String(),
			reqOrg:    vOrg2,
			reqUser:   vu2,
			respCode:  http.StatusBadRequest,
		},
		{
			name:      "success user has both Provider/Tenant role",
			reqSiteID: mst.ID.String(),
			reqOrg:    mOrg,
			reqUser:   mu,
			respCode:  http.StatusOK,
		},
		{
			name:      "success user has both Provider/Tenant role, user has Allocation for Site owned by another org",
			reqSiteID: st2.ID.String(),
			reqOrg:    mOrg2,
			reqUser:   mu2,
			respCode:  http.StatusOK,
		},
		{
			name:      "success user has targetedInstanceCreation capability but no Allocation",
			reqSiteID: st.ID.String(),
			reqOrg:    tnOrg3,
			reqUser:   tnu3,
			respCode:  http.StatusOK,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			path := fmt.Sprintf("/v2/org/%v/nico/site/%v/status-history?%s", tc.reqOrg, tc.reqSiteID, tc.query.Encode())
			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tc.reqOrg, tc.reqSiteID)
			ec.Set("user", tc.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			assert.NoError(t, handler.Handle(ec))
			assert.Equal(t, tc.respCode, rec.Code)

			// only check the rest if the response code is OK
			if rec.Code == http.StatusOK {
				resp := []model.APIStatusDetail{}
				assert.Nil(t, json.Unmarshal(rec.Body.Bytes(), &resp))
				assert.Equal(t, 20, len(resp)) // default page count is 20

				ph := rec.Header().Get(pagination.ResponseHeaderName)
				assert.NotEmpty(t, ph)

				pr := &pagination.PageResponse{}
				assert.NoError(t, json.Unmarshal([]byte(ph), pr))
				assert.Equal(t, totalCount, pr.Total)
			}
		})
	}
}
