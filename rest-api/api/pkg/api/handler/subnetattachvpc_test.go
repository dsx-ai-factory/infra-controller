// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type subnetAttachVpcFixture struct {
	dbSession       *cdb.Session
	org             string
	user            *cdbm.User
	tenant          *cdbm.Tenant
	provider        *cdbm.InfrastructureProvider
	site            *cdbm.Site
	sourceVpc       *cdbm.Vpc
	targetVpc       *cdbm.Vpc
	subnet          *cdbm.Subnet
	scp             *sc.ClientPool
	proxiedRequest  *grpcproxy.Request
	setCoreResponse func(*corev1.NetworkSegment)
}

func newSubnetAttachVpcFixture(t *testing.T, workflowErr error) subnetAttachVpcFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-tenant-org"
	user := common.TestBuildUser(t, dbSession, uuid.NewString(), org, []string{authz.TenantAdminRole})
	providerUser := common.TestBuildUser(t, dbSession, uuid.NewString(), "test-provider-org", []string{authz.ProviderAdminRole})
	provider := common.TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-provider-org", providerUser)
	site := common.TestBuildSite(t, dbSession, provider, "test-site", providerUser)
	var err error
	site, err = cdbm.NewSiteDAO(dbSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: site.ID, Status: cutil.GetPtr(cdbm.SiteStatusRegistered)})
	require.NoError(t, err)
	tenant := common.TestBuildTenant(t, dbSession, "test-tenant", org, user)
	sourceControllerVpcID := uuid.New()
	targetControllerVpcID := uuid.New()
	sourceVpc := common.TestBuildVPC(t, dbSession, "source-vpc", provider, tenant, site, &sourceControllerVpcID, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, user)
	targetVpc := common.TestBuildVPC(t, dbSession, "target-vpc", provider, tenant, site, &targetControllerVpcID, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, user)
	controllerSegmentID := uuid.New()
	subnet := common.TestBuildSubnet(t, dbSession, "test-subnet", tenant, sourceVpc, &controllerSegmentID, cdbm.SubnetStatusReady, user)

	coreResponse := subnetAttachVpcCoreResponse(controllerSegmentID, targetControllerVpcID)
	proxiedRequest := &grpcproxy.Request{}
	workflowRun := &tmocks.WorkflowRun{}
	workflowRun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		responseJSON, marshalErr := protojson.Marshal(coreResponse)
		require.NoError(t, marshalErr)
		args.Get(1).(*grpcproxy.Response).ResponseJSON = responseJSON
	}).Return(workflowErr)
	temporalClient := &tmocks.Client{}
	temporalClient.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.MatchedBy(func(request grpcproxy.Request) bool {
		*proxiedRequest = request
		return true
	})).Return(workflowRun, nil)
	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = temporalClient

	return subnetAttachVpcFixture{
		dbSession: dbSession, org: org, user: user, tenant: tenant, provider: provider, site: site,
		sourceVpc: sourceVpc, targetVpc: targetVpc, subnet: subnet, scp: scp,
		proxiedRequest: proxiedRequest,
		setCoreResponse: func(response *corev1.NetworkSegment) {
			coreResponse = response
		},
	}
}

func subnetAttachVpcCoreResponse(segmentID, vpcID uuid.UUID) *corev1.NetworkSegment {
	return &corev1.NetworkSegment{
		Id: &corev1.NetworkSegmentId{Value: segmentID.String()},
		Config: &corev1.NetworkSegmentConfig{
			SegmentType: corev1.NetworkSegmentType_TENANT,
			VpcId:       &corev1.VpcId{Value: vpcID.String()},
		},
	}
}

func (f subnetAttachVpcFixture) request(t *testing.T, user *cdbm.User, subnetID string, body string) *httptest.ResponseRecorder {
	t.Helper()

	req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(body))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := echo.New().NewContext(req, rec)
	ec.SetParamNames("orgName", "subnetId")
	ec.SetParamValues(f.org, subnetID)
	if user != nil {
		ec.Set("user", user)
	}
	require.NoError(t, NewAttachSubnetVpcHandler(f.dbSession, f.scp).Handle(ec))
	return rec
}

func TestAttachSubnetVpcHandler_Handle(t *testing.T) {
	timeoutErr := tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil, nil)
	tests := []struct {
		name               string
		workflowErr        error
		prepare            func(*testing.T, *subnetAttachVpcFixture) (string, string, *cdbm.User)
		expectedStatus     int
		expectedVpc        string
		expectProxyRequest bool
	}{
		{
			name: "reassigns an unallocated ETV Subnet after explicit acknowledgement",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String(), AllowReplace: true})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus:     http.StatusOK,
			expectedVpc:        "target",
			expectProxyRequest: true,
		},
		{
			name: "keeps same-target retry idempotent without replacement acknowledgement",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				fixture.setCoreResponse(subnetAttachVpcCoreResponse(*fixture.subnet.ControllerNetworkSegmentID, *fixture.sourceVpc.ControllerVpcID))
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.sourceVpc.ID.String()})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus:     http.StatusOK,
			expectedVpc:        "source",
			expectProxyRequest: true,
		},
		{
			name: "reassigns between VPCs using the same NVUE mode",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				for _, vpc := range []*cdbm.Vpc{fixture.sourceVpc, fixture.targetVpc} {
					_, err := cdbm.NewVpcDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.VpcUpdateInput{
						VpcID: vpc.ID, NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizerWithNVUE),
					})
					require.NoError(t, err)
				}
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String(), AllowReplace: true})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus:     http.StatusOK,
			expectedVpc:        "target",
			expectProxyRequest: true,
		},
		{
			name: "treats legacy untyped source and target VPCs as Ethernet virtualizers",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				_, err := fixture.dbSession.DB.ExecContext(
					context.Background(),
					"UPDATE vpc SET network_virtualization_type = NULL WHERE id IN (?, ?)",
					fixture.sourceVpc.ID,
					fixture.targetVpc.ID,
				)
				require.NoError(t, err)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String(), AllowReplace: true})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus:     http.StatusOK,
			expectedVpc:        "target",
			expectProxyRequest: true,
		},
		{
			name: "rejects moving an Ethernet virtualizer Subnet to an NVUE VPC",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				_, err := cdbm.NewVpcDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.VpcUpdateInput{
					VpcID: fixture.targetVpc.ID, NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizerWithNVUE),
				})
				require.NoError(t, err)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String()})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus: http.StatusBadRequest,
			expectedVpc:    "source",
		},
		{
			name: "rejects moving an NVUE Subnet to an Ethernet virtualizer VPC",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				_, err := cdbm.NewVpcDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.VpcUpdateInput{
					VpcID: fixture.sourceVpc.ID, NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizerWithNVUE),
				})
				require.NoError(t, err)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String()})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus: http.StatusBadRequest,
			expectedVpc:    "source",
		},
		{
			name: "delegates stale Subnet lifecycle state to Core",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				_, err := cdbm.NewSubnetDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.SubnetUpdateInput{
					SubnetId: fixture.subnet.ID, Status: cutil.GetPtr(cdbm.SubnetStatusError), IsMissingOnSite: cutil.GetPtr(true),
				})
				require.NoError(t, err)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String(), AllowReplace: true})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus:     http.StatusOK,
			expectedVpc:        "target",
			expectProxyRequest: true,
		},
		{
			name: "delegates stale source VPC lifecycle state to Core",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				_, err := cdbm.NewVpcDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.VpcUpdateInput{
					VpcID: fixture.sourceVpc.ID, Status: cutil.GetPtr(cdbm.VpcStatusError),
				})
				require.NoError(t, err)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String(), AllowReplace: true})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus:     http.StatusOK,
			expectedVpc:        "target",
			expectProxyRequest: true,
		},
		{
			name: "delegates stale target VPC lifecycle state to Core",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				_, err := cdbm.NewVpcDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.VpcUpdateInput{
					VpcID: fixture.targetVpc.ID, Status: cutil.GetPtr(cdbm.VpcStatusError),
				})
				require.NoError(t, err)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String(), AllowReplace: true})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus:     http.StatusOK,
			expectedVpc:        "target",
			expectProxyRequest: true,
		},
		{
			name: "rejects an FNN target before calling Core",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				controllerVpcID := uuid.New()
				fnnVpc := common.TestBuildVPC(t, fixture.dbSession, "fnn-vpc", fixture.provider, fixture.tenant, fixture.site, &controllerVpcID, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, fixture.user)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fnnVpc.ID.String()})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus: http.StatusBadRequest,
			expectedVpc:    "source",
		},
		{
			name: "rejects a Subnet in an FNN source VPC before calling Core",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				_, err := cdbm.NewVpcDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.VpcUpdateInput{
					VpcID: fixture.sourceVpc.ID, NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
				})
				require.NoError(t, err)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String()})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus: http.StatusBadRequest,
			expectedVpc:    "source",
		},
		{
			name: "rejects another Tenant's target VPC before calling Core",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				otherTenant := common.TestBuildTenant(t, fixture.dbSession, "other-tenant", "other-org", fixture.user)
				controllerVpcID := uuid.New()
				otherVpc := common.TestBuildVPC(t, fixture.dbSession, "other-tenant-vpc", fixture.provider, otherTenant, fixture.site, &controllerVpcID, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, fixture.user)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: otherVpc.ID.String()})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus: http.StatusBadRequest,
			expectedVpc:    "source",
		},
		{
			name: "rejects a target VPC at another Site before calling Core",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				otherSite := common.TestBuildSite(t, fixture.dbSession, fixture.provider, "other-site", fixture.user)
				controllerVpcID := uuid.New()
				otherVpc := common.TestBuildVPC(t, fixture.dbSession, "other-site-vpc", fixture.provider, fixture.tenant, otherSite, &controllerVpcID, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, fixture.user)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: otherVpc.ID.String()})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus: http.StatusBadRequest,
			expectedVpc:    "source",
		},
		{
			name: "rejects an allocated Subnet before calling Core",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				instance := &cdbm.Instance{
					ID: uuid.New(), Name: "test-instance", TenantID: fixture.tenant.ID,
					InfrastructureProviderID: fixture.provider.ID, SiteID: fixture.site.ID,
					VpcID: fixture.sourceVpc.ID, Status: cdbm.InstanceStatusReady, CreatedBy: fixture.user.ID,
				}
				_, err := fixture.dbSession.DB.NewInsert().Model(instance).Exec(context.Background())
				require.NoError(t, err)
				_, err = fixture.dbSession.DB.NewInsert().Model(&cdbm.Interface{
					ID: uuid.New(), InstanceID: instance.ID, SubnetID: &fixture.subnet.ID,
					Status: cdbm.InterfaceStatusReady, CreatedBy: fixture.user.ID,
				}).Exec(context.Background())
				require.NoError(t, err)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String()})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus: http.StatusBadRequest,
			expectedVpc:    "source",
		},
		{
			name:        "leaves REST VPC unchanged when the Core proxy times out",
			workflowErr: timeoutErr,
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String(), AllowReplace: true})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus:     http.StatusGatewayTimeout,
			expectedVpc:        "source",
			expectProxyRequest: true,
		},
		{
			name: "leaves REST VPC unchanged for an inconsistent Core response",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				fixture.setCoreResponse(subnetAttachVpcCoreResponse(*fixture.subnet.ControllerNetworkSegmentID, *fixture.sourceVpc.ControllerVpcID))
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String(), AllowReplace: true})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus:     http.StatusInternalServerError,
			expectedVpc:        "source",
			expectProxyRequest: true,
		},
		{
			name: "leaves REST VPC unchanged when Core returns a non-tenant segment",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				response := subnetAttachVpcCoreResponse(*fixture.subnet.ControllerNetworkSegmentID, *fixture.targetVpc.ControllerVpcID)
				response.Config.SegmentType = corev1.NetworkSegmentType_HOST_INBAND
				fixture.setCoreResponse(response)
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String(), AllowReplace: true})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), fixture.user
			},
			expectedStatus:     http.StatusInternalServerError,
			expectedVpc:        "source",
			expectProxyRequest: true,
		},
		{
			name: "rejects an invalid Subnet path ID",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String()})
				require.NoError(t, err)
				return "invalid", string(body), fixture.user
			},
			expectedStatus: http.StatusBadRequest,
			expectedVpc:    "source",
		},
		{
			name: "rejects a missing request user",
			prepare: func(t *testing.T, fixture *subnetAttachVpcFixture) (string, string, *cdbm.User) {
				body, err := json.Marshal(model.APISubnetAttachVpcRequest{VpcID: fixture.targetVpc.ID.String()})
				require.NoError(t, err)
				return fixture.subnet.ID.String(), string(body), nil
			},
			expectedStatus: http.StatusInternalServerError,
			expectedVpc:    "source",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			fixture := newSubnetAttachVpcFixture(t, test.workflowErr)
			subnetID, body, user := test.prepare(t, &fixture)
			response := fixture.request(t, user, subnetID, body)
			assert.Equal(t, test.expectedStatus, response.Code)

			updatedSubnet, err := cdbm.NewSubnetDAO(fixture.dbSession).GetByID(context.Background(), nil, fixture.subnet.ID, nil)
			require.NoError(t, err)
			expectedVpcID := fixture.sourceVpc.ID
			if test.expectedVpc == "target" {
				expectedVpcID = fixture.targetVpc.ID
			}
			assert.Equal(t, expectedVpcID, updatedSubnet.VpcID)

			if test.expectProxyRequest {
				assert.Equal(t, corev1.Forge_AttachNetworkSegmentToVpc_FullMethodName, fixture.proxiedRequest.FullMethod)
				var coreRequest corev1.AttachNetworkSegmentToVpcRequest
				require.NoError(t, protojson.Unmarshal(fixture.proxiedRequest.RequestJSON, &coreRequest))
				assert.Equal(t, fixture.subnet.ControllerNetworkSegmentID.String(), coreRequest.GetNetworkSegmentId().GetValue())
				expectedControllerVpcID := fixture.sourceVpc.ControllerVpcID
				if test.expectedVpc == "target" || test.expectedStatus != http.StatusOK {
					expectedControllerVpcID = fixture.targetVpc.ControllerVpcID
				}
				require.NotNil(t, expectedControllerVpcID)
				assert.Equal(t, expectedControllerVpcID.String(), coreRequest.GetVpcId().GetValue())
				var submittedRequest model.APISubnetAttachVpcRequest
				require.NoError(t, json.Unmarshal([]byte(body), &submittedRequest))
				assert.Equal(t, submittedRequest.AllowReplace, coreRequest.GetAllowReplace())
			} else {
				assert.Empty(t, fixture.proxiedRequest.FullMethod)
			}

			if response.Code == http.StatusOK {
				var apiSubnet model.APISubnet
				require.NoError(t, json.Unmarshal(response.Body.Bytes(), &apiSubnet))
				assert.Equal(t, expectedVpcID.String(), apiSubnet.VpcID)
			}
		})
	}
}
