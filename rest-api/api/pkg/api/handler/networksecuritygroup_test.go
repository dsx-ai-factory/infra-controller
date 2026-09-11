// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
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

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"

	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	"github.com/stretchr/testify/mock"
	"go.temporal.io/api/enums/v1"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
)

func getIntPtrToUint32Ptr(i *int) *uint32 {
	if i == nil {
		return nil
	}

	i32 := uint32(*i)

	return &i32
}

func testBuildNetworkSecurityGroup(t *testing.T, dbSession *cdb.Session, name string, tenant *cdbm.Tenant, site *cdbm.Site, status string) *cdbm.NetworkSecurityGroup {
	nsg := &cdbm.NetworkSecurityGroup{
		ID:        uuid.NewString(),
		Name:      name,
		SiteID:    site.ID,
		TenantID:  tenant.ID,
		TenantOrg: tenant.Org,
		Status:    status,
	}
	_, err := dbSession.DB.NewInsert().Model(nsg).Exec(context.Background())
	assert.Nil(t, err)
	return nsg
}

func testUpdateNetworkSecurityGroup(t *testing.T, dbSession *cdb.Session, nsg *cdbm.NetworkSecurityGroup) *cdbm.NetworkSecurityGroup {

	_, err := dbSession.DB.NewUpdate().Where("id = ?", nsg.ID).Model(nsg).Exec(context.Background())
	assert.Nil(t, err)
	return nsg
}

func testNetworkSecurityGroupSetupSchema(t *testing.T, dbSession *cdb.Session) {
	testInstanceSetupSchema(t, dbSession)
	// create Security Group table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
}

func testCreateNetworkSecurityGroup(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, tn *cdbm.Tenant) *cdbm.NetworkSecurityGroup {
	nsgDAO := cdbm.NewNetworkSecurityGroupDAO(dbSession)

	nsg, err := nsgDAO.Create(context.Background(), nil, cdbm.NetworkSecurityGroupCreateInput{
		Name:      name,
		SiteID:    site.ID,
		TenantID:  tn.ID,
		TenantOrg: tn.Org,
		Rules:     []*cdbm.NetworkSecurityGroupRule{},
	})
	assert.Nil(t, err)

	return nsg
}

func TestNewCreateNetworkSecurityGroupHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	tests := []struct {
		name string
		args args
		want CreateNetworkSecurityGroupHandler
	}{
		{
			name: "test CreateNetworkSecurityGroupHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: CreateNetworkSecurityGroupHandler{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewCreateNetworkSecurityGroupHandler(tt.args.dbSession, tt.args.tc, scp, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("CreateNetworkSecurityGroupHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNetworkSecurityGroupHandler_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testNetworkSecurityGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}

	tnOrg2 := "test-tenant-org-2"

	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)
	st := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)

	st.Config = &cdbm.SiteConfig{NetworkSecurityGroup: true}
	_ = testUpdateSite(t, dbSession, st)

	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)

	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	ts1s2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts1s2)

	// Tenant 2
	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles)
	tn2 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg2, tnu2)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg2, tn2.ID, st.ID, tnu2.ID)
	assert.NotNil(t, ts2)

	requestedNetworkSecurityGroupID := uuid.New()
	existingNetworkSecurityGroupID := uuid.New()
	deletedNetworkSecurityGroupID := uuid.New()
	nsgDAO := cdbm.NewNetworkSecurityGroupDAO(dbSession)
	_, err := nsgDAO.Create(ctx, nil, cdbm.NetworkSecurityGroupCreateInput{
		NetworkSecurityGroupID: cutil.GetPtr(existingNetworkSecurityGroupID.String()),
		Name:                   "existing-network-security-group-id",
		SiteID:                 st.ID,
		TenantID:               tn1.ID,
		TenantOrg:              tn1.Org,
		Status:                 cdbm.NetworkSecurityGroupStatusReady,
		Rules:                  []*cdbm.NetworkSecurityGroupRule{},
		CreatedByID:            tnu1.ID,
	})
	require.NoError(t, err)
	deletedNSG, err := nsgDAO.Create(ctx, nil, cdbm.NetworkSecurityGroupCreateInput{
		NetworkSecurityGroupID: cutil.GetPtr(deletedNetworkSecurityGroupID.String()),
		Name:                   "deleted-network-security-group-id",
		SiteID:                 st.ID,
		TenantID:               tn1.ID,
		TenantOrg:              tn1.Org,
		Status:                 cdbm.NetworkSecurityGroupStatusReady,
		Rules:                  []*cdbm.NetworkSecurityGroupRule{},
		CreatedByID:            tnu1.ID,
	})
	require.NoError(t, err)
	require.NoError(t, nsgDAO.Delete(ctx, nil, cdbm.NetworkSecurityGroupDeleteInput{
		NetworkSecurityGroupID: deletedNSG.ID,
		UpdatedByID:            tnu1.ID,
	}))

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateNetworkSecurityGroup", mock.Anything).Return(wrun, nil)

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[st.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateNetworkSecurityGroup", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// Unimplemented mocking
	//
	scpWithUnimplemented := sc.NewClientPool(tcfg)
	tscWithUnimplemented := &tmocks.Client{}

	scpWithUnimplemented.IDClientMap[st.ID.String()] = tscWithUnimplemented

	wrunUnimplemented := &tmocks.WorkflowRun{}
	wrunUnimplemented.On("GetID").Return("workflow-with-unimplemented")

	wrunUnimplemented.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithUnimplemented.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateNetworkSecurityGroup", mock.Anything).Return(wrunUnimplemented, nil)

	//
	// NICo unimplemented mocking
	//
	scpWithNICoUnimplemented := sc.NewClientPool(tcfg)
	tscWithNICoUnimplemented := &tmocks.Client{}

	scpWithNICoUnimplemented.IDClientMap[st.ID.String()] = tscWithNICoUnimplemented

	wrunWithNICoUnimplemented := &tmocks.WorkflowRun{}
	wrunWithNICoUnimplemented.On("GetID").Return("workflow-WithNICoUnimplemented")

	wrunWithNICoUnimplemented.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoUnimplemented, errors.New("NICo went bananas")))

	tscWithNICoUnimplemented.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateNetworkSecurityGroup", mock.Anything).Return(wrunWithNICoUnimplemented, nil)

	type params struct {
		org  string
		user *cdbm.User
	}
	type handlerConfig struct {
		dbSession      *cdb.Session
		temporalClient temporalClient.Client
		clientPool     *sc.ClientPool
		config         *config.Config
	}

	tests := []struct {
		name             string
		handlerConfig    handlerConfig
		params           params
		requestPayload   *model.APINetworkSecurityGroupCreateRequest
		wantErr          bool
		wantResponseCode int
	}{
		{
			name:             "test bad rules - failure",
			wantErr:          false,
			wantResponseCode: http.StatusBadRequest,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				Name:        "Spark VPC Firewall 2",
				Description: cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:      st.ID.String(),
				Rules: []model.APINetworkSecurityGroupRule{
					{
						Direction: "bad",
					},
				},
				Labels: map[string]string{
					"flavor": "coconut",
				},
			},
		},
		{
			name:             "test duplicate rule names - fail",
			wantErr:          false,
			wantResponseCode: http.StatusBadRequest,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				Name:        "Spark VPC Firewall",
				Description: cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:      st.ID.String(),
				Rules: []model.APINetworkSecurityGroupRule{
					{
						Name:                 cutil.GetPtr("anything"),
						Direction:            model.APINetworkSecurityGroupRuleDirectionIngress,
						SourcePortRange:      cutil.GetPtr("80-81"),
						DestinationPortRange: cutil.GetPtr("180-181"),
						Protocol:             model.APINetworkSecurityGroupRuleProtocolTcp,
						Action:               model.APINetworkSecurityGroupRuleActionPermit,
						SourcePrefix:         cutil.GetPtr("0.0.0.0/0"),
						DestinationPrefix:    cutil.GetPtr("1.1.1.1/0"),
						Priority:             55,
					},
					{
						Name:                 cutil.GetPtr("anything"),
						Direction:            model.APINetworkSecurityGroupRuleDirectionIngress,
						SourcePortRange:      cutil.GetPtr("80-81"),
						DestinationPortRange: cutil.GetPtr("180-181"),
						Protocol:             model.APINetworkSecurityGroupRuleProtocolTcp,
						Action:               model.APINetworkSecurityGroupRuleActionPermit,
						SourcePrefix:         cutil.GetPtr("0.0.0.0/0"),
						DestinationPrefix:    cutil.GetPtr("1.1.1.1/0"),
						Priority:             55,
					},
				},
				Labels: map[string]string{
					"flavor": "coconut",
				},
			},
		},
		{
			name:             "test good config - success",
			wantErr:          false,
			wantResponseCode: http.StatusCreated,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				Name:           "Spark VPC Firewall",
				Description:    cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:         st.ID.String(),
				StatefulEgress: true,
				Rules: []model.APINetworkSecurityGroupRule{
					{
						Direction:            model.APINetworkSecurityGroupRuleDirectionIngress,
						SourcePortRange:      cutil.GetPtr("80-81"),
						DestinationPortRange: cutil.GetPtr("180-181"),
						Protocol:             model.APINetworkSecurityGroupRuleProtocolTcp,
						Action:               model.APINetworkSecurityGroupRuleActionPermit,
						SourcePrefix:         cutil.GetPtr("0.0.0.0/0"),
						DestinationPrefix:    cutil.GetPtr("1.1.1.1/0"),
						Priority:             55,
					},
				},
				Labels: map[string]string{
					"flavor": "coconut",
				},
			},
		},
		{
			name:             "test good config with user-specified ID - success",
			wantErr:          false,
			wantResponseCode: http.StatusCreated,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				ID:             &requestedNetworkSecurityGroupID,
				Name:           "Spark VPC Firewall With User ID",
				Description:    cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:         st.ID.String(),
				StatefulEgress: true,
				Rules: []model.APINetworkSecurityGroupRule{
					{
						Direction:         model.APINetworkSecurityGroupRuleDirectionIngress,
						Protocol:          model.APINetworkSecurityGroupRuleProtocolTcp,
						Action:            model.APINetworkSecurityGroupRuleActionPermit,
						SourcePrefix:      cutil.GetPtr("0.0.0.0/0"),
						DestinationPrefix: cutil.GetPtr("1.1.1.1/0"),
					},
				},
			},
		},
		{
			name:             "test duplicate user-specified ID - fail",
			wantErr:          false,
			wantResponseCode: http.StatusConflict,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				ID:     &existingNetworkSecurityGroupID,
				Name:   "Spark VPC Firewall Existing ID",
				SiteID: st.ID.String(),
			},
		},
		{
			name:             "test soft-deleted user-specified ID - fail",
			wantErr:          false,
			wantResponseCode: http.StatusConflict,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				ID:     &deletedNetworkSecurityGroupID,
				Name:   "Spark VPC Firewall Deleted ID",
				SiteID: st.ID.String(),
			},
		},
		{
			name:             "test good config but site does not have NSG capability - fail",
			wantErr:          false,
			wantResponseCode: http.StatusPreconditionFailed,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				Name:        "Spark VPC Firewall",
				Description: cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:      st2.ID.String(),
				Rules: []model.APINetworkSecurityGroupRule{
					{
						Direction:            model.APINetworkSecurityGroupRuleDirectionIngress,
						SourcePortRange:      cutil.GetPtr("80-81"),
						DestinationPortRange: cutil.GetPtr("180-181"),
						Protocol:             model.APINetworkSecurityGroupRuleProtocolTcp,
						Action:               model.APINetworkSecurityGroupRuleActionPermit,
						SourcePrefix:         cutil.GetPtr("0.0.0.0/0"),
						DestinationPrefix:    cutil.GetPtr("1.1.1.1/0"),
						Priority:             55,
					},
				},
				Labels: map[string]string{
					"flavor": "coconut",
				},
			},
		},
		{
			name:             "test same good config, which now means duplicate name - fail",
			wantErr:          false,
			wantResponseCode: http.StatusConflict,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				Name:        "Spark VPC Firewall",
				Description: cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:      st.ID.String(),
				Rules: []model.APINetworkSecurityGroupRule{
					{
						Direction:            model.APINetworkSecurityGroupRuleDirectionIngress,
						SourcePortRange:      cutil.GetPtr("80-81"),
						DestinationPortRange: cutil.GetPtr("180-181"),
						Protocol:             model.APINetworkSecurityGroupRuleProtocolTcp,
						Action:               model.APINetworkSecurityGroupRuleActionPermit,
						SourcePrefix:         cutil.GetPtr("0.0.0.0/0"),
						DestinationPrefix:    cutil.GetPtr("1.1.1.1/0"),
					},
				},
			},
		},
		{
			name:             "test same good config, with a duplicate name but for a separate tenant - success",
			wantErr:          false,
			wantResponseCode: http.StatusCreated,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg2,
				user: tnu2,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				Name:        "Spark VPC Firewall",
				Description: cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:      st.ID.String(),
				Rules: []model.APINetworkSecurityGroupRule{
					{
						Direction:            model.APINetworkSecurityGroupRuleDirectionIngress,
						SourcePortRange:      cutil.GetPtr("80-81"),
						DestinationPortRange: cutil.GetPtr("180-181"),
						Protocol:             model.APINetworkSecurityGroupRuleProtocolTcp,
						Action:               model.APINetworkSecurityGroupRuleActionPermit,
						SourcePrefix:         cutil.GetPtr("0.0.0.0/0"),
						DestinationPrefix:    cutil.GetPtr("1.1.1.1/0"),
					},
				},
			},
		},
		{
			name:             "test good config but unimplemented - fail",
			wantErr:          false,
			wantResponseCode: http.StatusNotImplemented,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tscWithNICoUnimplemented,
				clientPool:     scpWithNICoUnimplemented,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				Name:        "Spark VPC Firewall 3000",
				Description: cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:      st.ID.String(),
				Rules: []model.APINetworkSecurityGroupRule{
					{
						Direction:            model.APINetworkSecurityGroupRuleDirectionIngress,
						SourcePortRange:      cutil.GetPtr("80-81"),
						DestinationPortRange: cutil.GetPtr("180-181"),
						Protocol:             model.APINetworkSecurityGroupRuleProtocolTcp,
						Action:               model.APINetworkSecurityGroupRuleActionPermit,
						SourcePrefix:         cutil.GetPtr("0.0.0.0/0"),
						DestinationPrefix:    cutil.GetPtr("1.1.1.1/0"),
					},
				},
			},
		},
		{
			name:             "test good config but timeout - fail",
			wantErr:          false,
			wantResponseCode: http.StatusInternalServerError,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tscWithTimeout,
				clientPool:     scpWithTimeout,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				Name:        "Spark VPC Firewall 3000",
				Description: cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:      st.ID.String(),
				Rules: []model.APINetworkSecurityGroupRule{
					{
						Direction:            model.APINetworkSecurityGroupRuleDirectionIngress,
						SourcePortRange:      cutil.GetPtr("80-81"),
						DestinationPortRange: cutil.GetPtr("180-181"),
						Protocol:             model.APINetworkSecurityGroupRuleProtocolTcp,
						Action:               model.APINetworkSecurityGroupRuleActionPermit,
						SourcePrefix:         cutil.GetPtr("0.0.0.0/0"),
						DestinationPrefix:    cutil.GetPtr("1.1.1.1/0"),
					},
				},
			},
		},
		{
			name:             "test bad config to validate rule processing is in place - fail",
			wantErr:          false,
			wantResponseCode: http.StatusBadRequest,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				Name:        "Spark VPC Firewall 4000",
				Description: cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:      st.ID.String(),
				Rules: []model.APINetworkSecurityGroupRule{
					{
						Direction: "to the moon",
						Protocol:  "carrier pigeon",
						Action:    "table-flip",
					},
				},
			},
		},
		{
			name:             "test missing name - fail",
			wantErr:          false,
			wantResponseCode: http.StatusBadRequest,
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			requestPayload: &model.APINetworkSecurityGroupCreateRequest{
				Name:        "",
				Description: cutil.GetPtr("Security policies for machines in Spark VPC"),
				SiteID:      st.ID.String(),
				Rules:       []model.APINetworkSecurityGroupRule{},
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			csh := CreateNetworkSecurityGroupHandler{
				dbSession: test.handlerConfig.dbSession,
				tc:        test.handlerConfig.temporalClient,
				scp:       test.handlerConfig.clientPool,
				cfg:       test.handlerConfig.config,
			}

			jsonData, _ := json.Marshal(test.requestPayload)

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(jsonData)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/network-security-group", test.params.org))
			ec.SetParamNames("orgName")
			ec.SetParamValues(test.params.org)
			ec.Set("user", test.params.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))
			err := csh.Handle(ec)

			assert.Equal(t, err != nil, test.wantErr)

			if err != nil {
				return
			}

			assert.Equal(t, test.wantResponseCode, rec.Code, rec.Body.String())

			if rec.Code != http.StatusCreated {
				return
			}

			rst := &model.APINetworkSecurityGroup{}

			err = json.Unmarshal(rec.Body.Bytes(), rst)

			if err != nil {
				t.Fatal(err)
			}

			//t.Errorf(string(jsonData))
			//t.Errorf(rec.Body.String())

			assert.Equal(t, test.requestPayload.SiteID, rst.SiteID)
			if test.requestPayload.ID != nil {
				assert.Equal(t, test.requestPayload.ID.String(), rst.ID)
			}

			assert.Equal(t, test.requestPayload.Name, rst.Name)

			if test.requestPayload.Description != nil {
				assert.Equal(t, *test.requestPayload.Description, *rst.Description)
			}

			if test.requestPayload.Labels != nil {
				e := reflect.DeepEqual(test.requestPayload.Labels, rst.Labels)
				if !e {
					t.Errorf("\n\n-------------- Expected --------------\n\n%+v\n\n--------------  Got --------------\n\n%+v", test.requestPayload.Labels, rst.Labels)
				}
			}

			if test.requestPayload.Rules != nil {
				for i, rule := range test.requestPayload.Rules {
					e := reflect.DeepEqual(&rule, rst.Rules[i])
					if !e {
						t.Errorf("\n\n-------------- Expected --------------\n\n%+v\n\n--------------  Got --------------\n\n%+v", &rule, rst.Rules[i])
					}
				}
			}
		})
	}
}

func TestNewGetAllNetworkSecurityGroupHandler(t *testing.T) {
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
		want GetAllNetworkSecurityGroupHandler
	}{
		{
			name: "test GetAllNetworkSecurityGroupHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetAllNetworkSecurityGroupHandler{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetAllNetworkSecurityGroupHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("GetAllNetworkSecurityGroupHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNetworkSecurityGroupHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testNetworkSecurityGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}

	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles2 := []string{authz.TenantAdminRole}

	// Providers
	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	// Sites
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)

	// Tenants and Site associations
	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	// Tenant 2
	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles2)
	tn2 := testInstanceBuildTenant(t, dbSession, "test-tenant2", tnOrg2, tnu2)

	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg2, tn2.ID, st1.ID, tnu2.ID)
	assert.NotNil(t, ts3)

	ts4 := testBuildTenantSiteAssociation(t, dbSession, tnOrg2, tn2.ID, st2.ID, tnu2.ID)
	assert.NotNil(t, ts4)

	// NetworkSecurityGroups

	rules := []*cdbm.NetworkSecurityGroupRule{
		&cdbm.NetworkSecurityGroupRule{
			NetworkSecurityGroupRuleAttributes: &corev1.NetworkSecurityGroupRuleAttributes{
				Id:             cutil.GetPtr(uuid.NewString()),
				Direction:      corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_EGRESS,
				Protocol:       corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
				Action:         corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_DENY,
				Priority:       55,
				Ipv6:           false,
				SrcPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(55)),
				SrcPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(56)),
				DstPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(57)),
				DstPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(58)),
				SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "0.0.0.0/0"},
				DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "1.1.1.1/0"},
			},
		},
		&cdbm.NetworkSecurityGroupRule{
			NetworkSecurityGroupRuleAttributes: &corev1.NetworkSecurityGroupRuleAttributes{
				Id:             cutil.GetPtr(uuid.NewString()),
				Direction:      corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_EGRESS,
				Protocol:       corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
				Action:         corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_DENY,
				Priority:       55,
				Ipv6:           false,
				SrcPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(55)),
				SrcPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(56)),
				DstPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(57)),
				DstPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(58)),
				SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "3.3.3.3/24"},
				DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "2.2.2.2/24"},
			},
		},
	}

	nsg1Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg1s1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	nsg1Site1.Rules = rules
	testUpdateNetworkSecurityGroup(t, dbSession, nsg1Site1)

	nsg2Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s2", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg2Site1)

	nsg1Site2 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s1", tn1, st2, cdbm.NetworkSecurityGroupStatusReady)
	nsg1Site2.Rules = rules
	testUpdateNetworkSecurityGroup(t, dbSession, nsg1Site2)

	nsg2Site2 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s2", tn1, st2, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg2Site2)

	nsg2Site3 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s3", tn1, st2, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg2Site3)

	// Create another tenant org + tenant with NSGs just to make sure
	// Get all doesn't pull up things it shouldn't
	nsg2Site1T2 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s2T2", tn2, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg2Site1T2)
	nsg2Site2T3 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s2T3", tn2, st2, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg2Site2T3)

	// VPCs

	vpc1Site1 := testVPCBuildVPC(t, dbSession, "vpc1Site1", ip, tn1, st1, nil, nil, map[string]string{}, cdbm.VpcStatusReady, tnu1)
	vpc1Site1.NetworkSecurityGroupID = cutil.GetPtr(nsg1Site1.ID)
	testUpdateVPC(t, dbSession, vpc1Site1)

	// Instances

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

	mc2 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc2)
	mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc2, ist1)
	assert.NotNil(t, mcinst2)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1Site1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	subnet2 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn1, vpc1Site1, nil, cdbm.SubnetStatusPending, tnu1)
	assert.NotNil(t, subnet2)

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	mci2 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc2.ID)
	assert.NotNil(t, mci2)

	inst1Site1Vpc1 := testInstanceBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1Site1.ID, cutil.GetPtr(mc1.ID), nil, nil, cdbm.InstanceStatusReady)
	inst1Site1Vpc1.NetworkSecurityGroupID = cutil.GetPtr(nsg1Site1.ID)
	testUpdateInstance(t, dbSession, inst1Site1Vpc1)

	inst2Site1Vpc1 := testInstanceBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1Site1.ID, cutil.GetPtr(mc1.ID), nil, nil, cdbm.InstanceStatusReady)
	inst2Site1Vpc1.NetworkSecurityGroupID = cutil.GetPtr(nsg1Site1.ID)
	testUpdateInstance(t, dbSession, inst2Site1Vpc1)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type params struct {
		org  string
		user *cdbm.User
	}

	type handlerConfig struct {
		dbSession      *cdb.Session
		temporalClient temporalClient.Client
		config         *config.Config
	}

	tests := []struct {
		name                string
		handlerConfig       handlerConfig
		params              params
		query               url.Values
		wantErr             bool
		wantResponseCode    int
		wantCount           *int
		wantFirstEntry      *cdbm.NetworkSecurityGroup
		wantFirstEntryStats *model.APINetworkSecurityGroupStats
	}{
		{
			name: "Get all across all sites for tenant - success",
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				config:         cfg,
			},
			query: url.Values{
				"status": []string{cdbm.NetworkSecurityGroupStatusReady},
			},
			wantCount:        cutil.GetPtr(5),
			wantResponseCode: http.StatusOK,
		},
		{
			name: "Get specific site for tenant - success",
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				config:         cfg,
			},
			query: url.Values{
				"siteId": []string{st2.ID.String()},
				"status": []string{cdbm.NetworkSecurityGroupStatusReady},
			},
			wantCount:        cutil.GetPtr(3),
			wantResponseCode: http.StatusOK,
			wantFirstEntry:   nsg1Site2,
		},
		{
			name: "Bad site ID",
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				config:         cfg,
			},
			query: url.Values{
				"siteId": []string{uuid.NewString()},
				"status": []string{cdbm.NetworkSecurityGroupStatusReady},
			},
			wantCount:        cutil.GetPtr(3),
			wantResponseCode: http.StatusBadRequest,
			wantFirstEntry:   nsg1Site2,
		},
		{
			name: "Really Bad site ID",
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				config:         cfg,
			},
			query: url.Values{
				"tenantId": []string{tn1.ID.String()},
				"siteId":   []string{"nonsense"},
				"status":   []string{cdbm.NetworkSecurityGroupStatusReady},
			},
			wantCount:        cutil.GetPtr(3),
			wantResponseCode: http.StatusBadRequest,
			wantFirstEntry:   nsg1Site2,
		},
		{
			name: "Get stats - success",
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				config:         cfg,
			},
			query: url.Values{
				"tenantId":               []string{tn1.ID.String()},
				"includeAttachmentStats": []string{"true"},
				"siteId":                 []string{st1.ID.String()},
				"status":                 []string{cdbm.NetworkSecurityGroupStatusReady},
			},
			wantResponseCode: http.StatusOK,
			wantFirstEntryStats: &model.APINetworkSecurityGroupStats{
				InUse:                   true,
				VpcAttachmentCount:      1,
				InstanceAttachmentCount: 2,
				TotalAttachmentCount:    3,
			},
		},
		{
			name: "Get stats when no attachments - success",
			params: params{
				org:  tnOrg,
				user: tnu1,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				config:         cfg,
			},
			query: url.Values{
				"tenantId":               []string{tn1.ID.String()},
				"includeAttachmentStats": []string{"true"},
				"siteId":                 []string{st2.ID.String()},
				"status":                 []string{cdbm.NetworkSecurityGroupStatusReady},
			},
			wantResponseCode: http.StatusOK,
			wantFirstEntryStats: &model.APINetworkSecurityGroupStats{
				InUse:                   false,
				VpcAttachmentCount:      0,
				InstanceAttachmentCount: 0,
				TotalAttachmentCount:    0,
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			csh := GetAllNetworkSecurityGroupHandler{
				dbSession: test.handlerConfig.dbSession,
				tc:        test.handlerConfig.temporalClient,
				cfg:       test.handlerConfig.config,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/network-security-group?%s", test.params.org, test.query.Encode())

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(test.params.org)
			ec.Set("user", test.params.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := csh.Handle(ec)
			require.NoError(t, err)

			assert.Equal(t, err != nil, test.wantErr)

			if err != nil {
				return
			}

			if !assert.Equal(t, test.wantResponseCode, rec.Code) {
				t.Errorf("GetAllNetworkSecurityGroupHandler.Handle() response = %s", rec.Body.String())
			}

			if rec.Code != http.StatusOK {
				return
			}

			rst := []*model.APINetworkSecurityGroup{}

			serr := json.Unmarshal(rec.Body.Bytes(), &rst)

			if test.wantCount != nil {
				assert.Equal(t, *test.wantCount, len(rst))
			}

			if test.wantFirstEntry != nil {
				assert.True(t, len(rst) > 0, "response has no records")
				assert.Equal(t, rst[0].ID, test.wantFirstEntry.ID)
			}

			if test.wantFirstEntryStats != nil {
				assert.True(t, len(rst) > 0, "response has no records")
				assert.Equal(t, test.wantFirstEntryStats, rst[0].AttachmentStats)
			}

			if serr != nil {
				t.Fatal(serr)
			}
		})
	}

}

func TestNewGetNetworkSecurityGroupHandler(t *testing.T) {
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
		want GetNetworkSecurityGroupHandler
	}{
		{
			name: "test GetNetworkSecurityGroupHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetNetworkSecurityGroupHandler{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetNetworkSecurityGroupHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("GetNetworkSecurityGroupHandler() = %+v, want %+v", got, tt.want)
			}
		})
	}
}

func TestNetworkSecurityGroupHandler_Get(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testNetworkSecurityGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}

	// Providers
	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	// Sites
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)

	// Tenants and Site associations
	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	// NetworkSecurityGroups

	rules := []*cdbm.NetworkSecurityGroupRule{
		&cdbm.NetworkSecurityGroupRule{
			NetworkSecurityGroupRuleAttributes: &corev1.NetworkSecurityGroupRuleAttributes{
				Id:             cutil.GetPtr(uuid.NewString()),
				Direction:      corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_EGRESS,
				Protocol:       corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
				Action:         corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_DENY,
				Priority:       55,
				Ipv6:           false,
				SrcPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(55)),
				SrcPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(56)),
				DstPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(57)),
				DstPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(58)),
				SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "0.0.0.0/0"},
				DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "1.1.1.1/0"},
			},
		},
		&cdbm.NetworkSecurityGroupRule{
			NetworkSecurityGroupRuleAttributes: &corev1.NetworkSecurityGroupRuleAttributes{
				Id:             cutil.GetPtr(uuid.NewString()),
				Direction:      corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_EGRESS,
				Protocol:       corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
				Action:         corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_DENY,
				Priority:       55,
				Ipv6:           false,
				SrcPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(55)),
				SrcPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(56)),
				DstPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(57)),
				DstPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(58)),
				SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "3.3.3.3/24"},
				DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "2.2.2.2/24"},
			},
		},
	}

	nsg1Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg1s1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	nsg1Site1.Rules = rules
	testUpdateNetworkSecurityGroup(t, dbSession, nsg1Site1)

	nsg2Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s2", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg2Site1)

	nsg1Site2 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s1", tn1, st2, cdbm.NetworkSecurityGroupStatusReady)
	nsg1Site2.Rules = rules
	testUpdateNetworkSecurityGroup(t, dbSession, nsg1Site2)

	nsg2Site2 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s2", tn1, st2, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg2Site2)

	// VPCs

	vpc1Site1 := testVPCBuildVPC(t, dbSession, "vpc1Site1", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc1Site1.NetworkSecurityGroupID = cutil.GetPtr(nsg1Site1.ID)
	testUpdateVPC(t, dbSession, vpc1Site1)

	// Instances

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

	mc2 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc2)
	mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc2, ist1)
	assert.NotNil(t, mcinst2)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1Site1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	subnet2 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn1, vpc1Site1, nil, cdbm.SubnetStatusPending, tnu1)
	assert.NotNil(t, subnet2)

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	mci2 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc2.ID)
	assert.NotNil(t, mci2)

	inst1Site1Vpc1 := testInstanceBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1Site1.ID, cutil.GetPtr(mc1.ID), nil, nil, cdbm.InstanceStatusReady)
	inst1Site1Vpc1.NetworkSecurityGroupID = cutil.GetPtr(nsg1Site1.ID)
	testUpdateInstance(t, dbSession, inst1Site1Vpc1)

	inst2Site1Vpc1 := testInstanceBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1Site1.ID, cutil.GetPtr(mc1.ID), nil, nil, cdbm.InstanceStatusReady)
	inst2Site1Vpc1.NetworkSecurityGroupID = cutil.GetPtr(nsg1Site1.ID)
	testUpdateInstance(t, dbSession, inst2Site1Vpc1)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type params struct {
		org                    string
		user                   *cdbm.User
		networkSecurityGroupID string
	}

	type handlerConfig struct {
		dbSession      *cdb.Session
		temporalClient temporalClient.Client
		config         *config.Config
	}

	tests := []struct {
		name             string
		handlerConfig    handlerConfig
		params           params
		query            url.Values
		wantErr          bool
		wantResponseCode int
		wantID           *string
		wantStats        *model.APINetworkSecurityGroupStats
	}{
		{
			name: "Get - success",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg1Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				config:         cfg,
			},
			query:            url.Values{},
			wantID:           cutil.GetPtr(nsg1Site1.ID),
			wantResponseCode: http.StatusOK,
		},
		{
			name: "Get stats - success",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg1Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				config:         cfg,
			},
			query: url.Values{
				"includeAttachmentStats": []string{"true"},
			},
			wantResponseCode: http.StatusOK,
			wantStats: &model.APINetworkSecurityGroupStats{
				InUse:                   true,
				VpcAttachmentCount:      1,
				InstanceAttachmentCount: 2,
				TotalAttachmentCount:    3,
			},
		},
		{
			name: "Bad NSG ID - fail",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: uuid.NewString(),
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				config:         cfg,
			},
			query:            url.Values{},
			wantID:           cutil.GetPtr(nsg1Site1.ID),
			wantResponseCode: http.StatusNotFound,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			csh := GetNetworkSecurityGroupHandler{
				dbSession: test.handlerConfig.dbSession,
				tc:        test.handlerConfig.temporalClient,
				cfg:       test.handlerConfig.config,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/network-security-group/%s?%s", test.params.org, test.params.networkSecurityGroupID, test.query.Encode())

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(test.params.org, test.params.networkSecurityGroupID)
			ec.Set("user", test.params.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := csh.Handle(ec)
			require.NoError(t, err)

			assert.Equal(t, err != nil, test.wantErr)

			if err != nil {
				return
			}

			if !assert.Equal(t, test.wantResponseCode, rec.Code) {
				t.Errorf("GetNetworkSecurityGroupHandler.Handle() for %s response = %s", test.params.networkSecurityGroupID, rec.Body.String())
			}

			if rec.Code != http.StatusOK {
				return
			}

			rst := &model.APINetworkSecurityGroup{}

			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Errorf("GetNetworkSecurityGroupHandler.Handle() for %s response = %s", test.params.networkSecurityGroupID, rec.Body.String())
				t.Fatal(serr)
			}

			if test.wantID != nil {
				assert.Equal(t, *test.wantID, rst.ID)
			}

			if test.wantStats != nil {
				assert.Equal(t, test.wantStats, rst.AttachmentStats)
			}

		})
	}

}

func TestNewDeleteNetworkSecurityGroupHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	tests := []struct {
		name string
		args args
		want DeleteNetworkSecurityGroupHandler
	}{
		{
			name: "test DeleteNetworkSecurityGroupHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: DeleteNetworkSecurityGroupHandler{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewDeleteNetworkSecurityGroupHandler(tt.args.dbSession, tt.args.tc, scp, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("DeleteNetworkSecurityGroupHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNetworkSecurityGroupHandler_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testNetworkSecurityGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}

	// Providers
	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	// Sites
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)

	// Tenants and Site associations
	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	// NetworkSecurityGroups

	nsg1Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg1s1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	testUpdateNetworkSecurityGroup(t, dbSession, nsg1Site1)

	nsg2Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg2Site1)

	nsg3Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg3s1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg3Site1)

	nsg4Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg4s1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg4Site1)

	nsg5Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg5s1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg5Site1)

	// VPCs

	vpc1Site1 := testVPCBuildVPC(t, dbSession, "vpc1Site1", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc1Site1.NetworkSecurityGroupID = cutil.GetPtr(nsg1Site1.ID)
	testUpdateVPC(t, dbSession, vpc1Site1)

	// Instances

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

	mc2 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc2)
	mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc2, ist1)
	assert.NotNil(t, mcinst2)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1Site1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	subnet2 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn1, vpc1Site1, nil, cdbm.SubnetStatusPending, tnu1)
	assert.NotNil(t, subnet2)

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	mci2 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc2.ID)
	assert.NotNil(t, mci2)

	inst1Site1Vpc1 := testInstanceBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1Site1.ID, cutil.GetPtr(mc1.ID), nil, nil, cdbm.InstanceStatusReady)
	inst1Site1Vpc1.NetworkSecurityGroupID = cutil.GetPtr(nsg2Site1.ID)
	testUpdateInstance(t, dbSession, inst1Site1Vpc1)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st1.ID.String()] = tsc
	scp.IDClientMap[st2.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteNetworkSecurityGroup", mock.Anything).Return(wrun, nil)

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[st1.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteNetworkSecurityGroup", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo not-found mocking
	//
	scpWithNICoNotFound := sc.NewClientPool(tcfg)
	tscWithNICoNotFound := &tmocks.Client{}

	scpWithNICoNotFound.IDClientMap[st1.ID.String()] = tscWithNICoNotFound

	wrunWithNICoNotFound := &tmocks.WorkflowRun{}
	wrunWithNICoNotFound.On("GetID").Return("workflow-WithNICoNotFound")

	wrunWithNICoNotFound.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoObjectNotFound, errors.New("NICo went bananas")))

	tscWithNICoNotFound.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteNetworkSecurityGroup", mock.Anything).Return(wrunWithNICoNotFound, nil)

	tscWithNICoNotFound.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo unimplemented mocking
	//
	scpWithNICoUnimplemented := sc.NewClientPool(tcfg)
	tscWithNICoUnimplemented := &tmocks.Client{}

	scpWithNICoUnimplemented.IDClientMap[st1.ID.String()] = tscWithNICoUnimplemented

	wrunWithNICoUnimplemented := &tmocks.WorkflowRun{}
	wrunWithNICoUnimplemented.On("GetID").Return("workflow-WithNICoUnimplemented")

	wrunWithNICoUnimplemented.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoUnimplemented, errors.New("NICo went bananas")))

	tscWithNICoUnimplemented.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteNetworkSecurityGroup", mock.Anything).Return(wrunWithNICoUnimplemented, nil)

	type params struct {
		org                    string
		user                   *cdbm.User
		networkSecurityGroupID string
	}
	type handlerConfig struct {
		dbSession      *cdb.Session
		temporalClient temporalClient.Client
		clientPool     *sc.ClientPool
		config         *config.Config
	}

	tests := []struct {
		name             string
		handlerConfig    handlerConfig
		params           params
		wantErr          bool
		wantResponseCode int
	}{
		{
			name: "Delete - success",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg4Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			wantResponseCode: http.StatusAccepted,
		},
		{
			name: "Delete but not found - success",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg5Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tscWithNICoNotFound,
				clientPool:     scpWithNICoNotFound,
				config:         cfg,
			},
			wantResponseCode: http.StatusAccepted,
		},
		{
			name: "Delete but timeout - fail",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg3Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tscWithTimeout,
				clientPool:     scpWithTimeout,
				config:         cfg,
			},
			wantResponseCode: http.StatusInternalServerError,
		},
		{
			name: "Delete but unimplemented - fail",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg3Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tscWithNICoUnimplemented,
				clientPool:     scpWithNICoUnimplemented,
				config:         cfg,
			},
			wantResponseCode: http.StatusNotImplemented,
		},
		{
			name: "Delete NSG in use on Instances - fail",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg1Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			wantResponseCode: http.StatusPreconditionFailed,
		},
		{
			name: "Delete NSG in use on VPCs - fail",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg2Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			wantResponseCode: http.StatusPreconditionFailed,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			dnsg := DeleteNetworkSecurityGroupHandler{
				dbSession: test.handlerConfig.dbSession,
				tc:        test.handlerConfig.temporalClient,
				scp:       test.handlerConfig.clientPool,
				cfg:       test.handlerConfig.config,
			}

			// Setup echo server/context

			path := fmt.Sprintf("/v2/org/%s/nico/network-security-group/%s", test.params.org, test.params.networkSecurityGroupID)

			req := httptest.NewRequest(http.MethodDelete, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(test.params.org, test.params.networkSecurityGroupID)
			ec.Set("user", test.params.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := dnsg.Handle(ec)
			require.NoError(t, err)

			assert.Equal(t, err != nil, test.wantErr)

			if err != nil {
				return
			}

			if !assert.Equal(t, test.wantResponseCode, rec.Code) {
				t.Errorf("DeleteNetworkSecurityGroupHandler.Handle() response = %s", rec.Body.String())
			}

		})
	}
}

func TestNewUpdateNetworkSecurityGroupHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	tests := []struct {
		name string
		args args
		want UpdateNetworkSecurityGroupHandler
	}{
		{
			name: "test UpdateNetworkSecurityGroupHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: UpdateNetworkSecurityGroupHandler{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewUpdateNetworkSecurityGroupHandler(tt.args.dbSession, tt.args.tc, scp, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("UpdateNetworkSecurityGroupHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNetworkSecurityGroupHandler_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	dbRules := []*cdbm.NetworkSecurityGroupRule{
		&cdbm.NetworkSecurityGroupRule{
			NetworkSecurityGroupRuleAttributes: &corev1.NetworkSecurityGroupRuleAttributes{
				Id:             cutil.GetPtr(uuid.NewString()),
				Direction:      corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_EGRESS,
				Protocol:       corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
				Action:         corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_DENY,
				Priority:       55,
				Ipv6:           false,
				SrcPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(55)),
				SrcPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(56)),
				DstPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(57)),
				DstPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(58)),
				SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "0.0.0.0/0"},
				DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "1.1.1.1/0"},
			},
		},
		&cdbm.NetworkSecurityGroupRule{
			NetworkSecurityGroupRuleAttributes: &corev1.NetworkSecurityGroupRuleAttributes{
				Id:             cutil.GetPtr(uuid.NewString()),
				Direction:      corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_EGRESS,
				Protocol:       corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_TCP,
				Action:         corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_DENY,
				Priority:       55,
				Ipv6:           false,
				SrcPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(55)),
				SrcPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(56)),
				DstPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(57)),
				DstPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(58)),
				SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "3.3.3.3/24"},
				DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "2.2.2.2/24"},
			},
		},
	}

	rules := []model.APINetworkSecurityGroupRule{}

	for _, rule := range dbRules {
		r := model.NewAPINetworkSecurityGroupRule(rule.NetworkSecurityGroupRuleAttributes)
		rules = append(rules, *r)
	}

	// A bad rule with no values.
	badRules := []model.APINetworkSecurityGroupRule{{}}

	testNetworkSecurityGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}

	// Providers
	ipu := testInstanceBuildUser(t, dbSession, uuid.New().String(), ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	// Sites
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)

	st1.Config = &cdbm.SiteConfig{NetworkSecurityGroup: true}
	_ = testUpdateSite(t, dbSession, st1)

	st2.Config = &cdbm.SiteConfig{NetworkSecurityGroup: true}
	_ = testUpdateSite(t, dbSession, st2)

	// Tenants and Site associations
	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	// NetworkSecurityGroups

	nsg1Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg1s1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	testUpdateNetworkSecurityGroup(t, dbSession, nsg1Site1)

	nsg2Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s2", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg2Site1)

	nsg3Site1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2s2", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg3Site1)

	// VPCs

	vpc1Site1 := testVPCBuildVPC(t, dbSession, "vpc1Site1", ip, tn1, st1, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	vpc1Site1.NetworkSecurityGroupID = cutil.GetPtr(nsg1Site1.ID)
	testUpdateVPC(t, dbSession, vpc1Site1)

	// Instances

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

	mc2 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc2)
	mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc2, ist1)
	assert.NotNil(t, mcinst2)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1Site1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	subnet2 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn1, vpc1Site1, nil, cdbm.SubnetStatusPending, tnu1)
	assert.NotNil(t, subnet2)

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	mci2 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc2.ID)
	assert.NotNil(t, mci2)

	inst1Site1Vpc1 := testInstanceBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1Site1.ID, cutil.GetPtr(mc1.ID), nil, nil, cdbm.InstanceStatusReady)
	inst1Site1Vpc1.NetworkSecurityGroupID = cutil.GetPtr(nsg2Site1.ID)
	testUpdateInstance(t, dbSession, inst1Site1Vpc1)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st1.ID.String()] = tsc
	scp.IDClientMap[st2.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateNetworkSecurityGroup", mock.Anything).Return(wrun, nil)

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[st1.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateNetworkSecurityGroup", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo unimplemented mocking
	//
	scpWithNICoUnimplemented := sc.NewClientPool(tcfg)
	tscWithNICoUnimplemented := &tmocks.Client{}

	scpWithNICoUnimplemented.IDClientMap[st1.ID.String()] = tscWithNICoUnimplemented

	wrunWithNICoUnimplemented := &tmocks.WorkflowRun{}
	wrunWithNICoUnimplemented.On("GetID").Return("workflow-WithNICoUnimplemented")

	wrunWithNICoUnimplemented.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoUnimplemented, errors.New("NICo went bananas")))

	tscWithNICoUnimplemented.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateNetworkSecurityGroup", mock.Anything).Return(wrunWithNICoUnimplemented, nil)

	type params struct {
		org                    string
		user                   *cdbm.User
		networkSecurityGroupID string
	}
	type handlerConfig struct {
		dbSession      *cdb.Session
		temporalClient temporalClient.Client
		clientPool     *sc.ClientPool
		config         *config.Config
	}

	tests := []struct {
		name             string
		handlerConfig    handlerConfig
		params           params
		requestPayload   *model.APINetworkSecurityGroupUpdateRequest
		wantErr          bool
		wantResponseCode int
		expectRules      bool
	}{
		{
			name: "Update - success",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg1Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			requestPayload: &model.APINetworkSecurityGroupUpdateRequest{
				StatefulEgress: cutil.GetPtr(true),
				Rules:          rules,
			},
			wantResponseCode: http.StatusOK,
		},
		{
			name: "Update duplicate rule names - fail",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg1Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			requestPayload: &model.APINetworkSecurityGroupUpdateRequest{
				Rules: append(rules, rules...),
			},
			wantResponseCode: http.StatusBadRequest,
		},
		{
			name: "Update just name but expect rules to remain - success",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg1Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			requestPayload: &model.APINetworkSecurityGroupUpdateRequest{
				Name: cutil.GetPtr("hello!"),
			},
			wantResponseCode: http.StatusOK,
			expectRules:      true,
		},
		{
			name: "Update but duplicate name but the same NSg - success",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg1Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			requestPayload: &model.APINetworkSecurityGroupUpdateRequest{
				Name: cutil.GetPtr(nsg1Site1.Name),
			},
			wantResponseCode: http.StatusOK,
		},
		{
			name: "Update but duplicate name - fail",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg1Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			requestPayload: &model.APINetworkSecurityGroupUpdateRequest{
				Name: cutil.GetPtr(nsg2Site1.Name),
			},
			wantResponseCode: http.StatusConflict,
		},
		{
			name: "Update with a bad rule - fail",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg1Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tc,
				clientPool:     scp,
				config:         cfg,
			},
			requestPayload: &model.APINetworkSecurityGroupUpdateRequest{
				Rules: badRules,
			},
			wantResponseCode: http.StatusBadRequest,
		},
		{
			name: "Update but timeout - fail",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg3Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tscWithTimeout,
				clientPool:     scpWithTimeout,
				config:         cfg,
			},
			requestPayload: &model.APINetworkSecurityGroupUpdateRequest{
				Rules: rules,
			},
			wantResponseCode: http.StatusInternalServerError,
		},
		{
			name: "Update but unimplemented - fail",
			params: params{
				org:                    tnOrg,
				user:                   tnu1,
				networkSecurityGroupID: nsg3Site1.ID,
			},
			handlerConfig: handlerConfig{
				dbSession:      dbSession,
				temporalClient: tscWithNICoUnimplemented,
				clientPool:     scpWithNICoUnimplemented,
				config:         cfg,
			},
			requestPayload: &model.APINetworkSecurityGroupUpdateRequest{
				Rules: rules,
			},
			wantResponseCode: http.StatusNotImplemented,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			unsgh := UpdateNetworkSecurityGroupHandler{
				dbSession: test.handlerConfig.dbSession,
				tc:        test.handlerConfig.temporalClient,
				scp:       test.handlerConfig.clientPool,
				cfg:       test.handlerConfig.config,
			}

			// Setup echo server/context

			path := fmt.Sprintf("/v2/org/%s/nico/network-security-group/%s", test.params.org, test.params.networkSecurityGroupID)

			jsonData, _ := json.Marshal(test.requestPayload)

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(string(jsonData)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetPath(path)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(test.params.org, test.params.networkSecurityGroupID)
			ec.Set("user", test.params.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))
			err := unsgh.Handle(ec)

			assert.Equal(t, err != nil, test.wantErr)

			if err != nil {
				return
			}

			if !assert.Equal(t, test.wantResponseCode, rec.Code) {
				t.Errorf("UpdateNetworkSecurityGroupHandler.Handle() response = %s", rec.Body.String())
			}

			if rec.Code != http.StatusOK {
				return
			}

			rst := &model.APINetworkSecurityGroup{}

			err = json.Unmarshal(rec.Body.Bytes(), rst)

			if err != nil {
				t.Fatal(err)
			}

			//t.Errorf(string(jsonData))
			//t.Errorf(rec.Body.String())
			if test.requestPayload.Name != nil {
				assert.Equal(t, *test.requestPayload.Name, rst.Name)
			}

			if test.requestPayload.Description != nil {
				assert.Equal(t, *test.requestPayload.Description, *rst.Description)
			}

			if test.requestPayload.Labels != nil {
				e := reflect.DeepEqual(test.requestPayload.Labels, rst.Labels)
				if !e {
					t.Errorf("\n\n-------------- Expected --------------\n\n%+v\n\n--------------  Got --------------\n\n%+v", test.requestPayload.Labels, rst.Labels)
				}
			}

			assert.True(t, test.requestPayload.StatefulEgress == nil || *test.requestPayload.StatefulEgress == rst.StatefulEgress)

			if test.requestPayload.Rules != nil {
				for i, rule := range test.requestPayload.Rules {
					e := reflect.DeepEqual(&rule, rst.Rules[i])
					if !e {
						t.Errorf("\n\n-------------- Expected --------------\n\n%+v\n\n--------------  Got --------------\n\n%+v", &rule, rst.Rules[i])
					}
				}
			}

			assert.True(t, test.expectRules == false || len(rst.Rules) > 0)
		})
	}
}
