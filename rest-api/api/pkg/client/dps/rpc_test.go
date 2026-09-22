// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dps

import (
	"context"
	"errors"
	"net"
	"testing"
	"time"

	dpsv1 "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps/internal/dpssdk/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/grpc/test/bufconn"
	"google.golang.org/protobuf/types/known/emptypb"
)

type testDPSServer struct {
	dpsv1.UnimplementedResourceGroupManagementServiceServer
	dpsv1.UnimplementedTopologyManagementServiceServer

	policies         []string
	policyCalls      int
	policyErrors     []error
	validateCalls    int
	validateErrors   []error
	validateRequest  *dpsv1.ValidateAllocationRequest
	validationResult *dpsv1.ValidateAllocationResponse_AllocationValidationResult
	createRequest    *dpsv1.ResourceGroupCreateRequest
	createError      error
	addRequest       *dpsv1.ResourceGroupAddResourcesRequest
	updateRequest    *dpsv1.ResourceGroupUpdateResourcesRequest
	removeRequest    *dpsv1.ResourceGroupRemoveResourcesRequest
	activateRequest  *dpsv1.ActivateResourceGroupRequest
	deleteRequest    *dpsv1.ResourceGroupDeleteRequest
	activateGroup    string
	deleteGroup      string
	deleteError      error
	activateError    error
	responseStatus   *dpsv1.Status
}

func (s *testDPSServer) ListPolicies(_ *emptypb.Empty, stream dpsv1.TopologyManagementService_ListPoliciesServer) error {
	s.policyCalls++
	if s.policyCalls <= len(s.policyErrors) && s.policyErrors[s.policyCalls-1] != nil {
		return s.policyErrors[s.policyCalls-1]
	}
	for _, name := range s.policies {
		err := stream.Send(&dpsv1.PolicyObject{Name: name})
		if err != nil {
			return err
		}
	}
	return nil
}

func (s *testDPSServer) ValidateAllocation(request *dpsv1.ValidateAllocationRequest, stream dpsv1.TopologyManagementService_ValidateAllocationServer) error {
	s.validateCalls++
	s.validateRequest = request
	if s.validateCalls <= len(s.validateErrors) && s.validateErrors[s.validateCalls-1] != nil {
		return s.validateErrors[s.validateCalls-1]
	}
	if s.validationResult == nil {
		return nil
	}
	return stream.Send(&dpsv1.ValidateAllocationResponse{
		Response: &dpsv1.ValidateAllocationResponse_AllocationValidationResult_{
			AllocationValidationResult: s.validationResult,
		},
	})
}

func (s *testDPSServer) ResourceGroupCreate(_ context.Context, request *dpsv1.ResourceGroupCreateRequest) (*dpsv1.ResourceGroupCreateResponse, error) {
	s.createRequest = request
	if s.createError != nil {
		return nil, s.createError
	}
	return &dpsv1.ResourceGroupCreateResponse{Status: s.okStatus()}, nil
}

func (s *testDPSServer) ResourceGroupDelete(_ context.Context, request *dpsv1.ResourceGroupDeleteRequest) (*dpsv1.ResourceGroupDeleteResponse, error) {
	s.deleteRequest = request
	s.deleteGroup = request.GetGroupName()
	if s.deleteError != nil {
		return nil, s.deleteError
	}
	return &dpsv1.ResourceGroupDeleteResponse{Status: s.okStatus()}, nil
}

func (s *testDPSServer) ResourceGroupAddResources(_ context.Context, request *dpsv1.ResourceGroupAddResourcesRequest) (*dpsv1.ResourceGroupAddResourcesResponse, error) {
	s.addRequest = request
	return &dpsv1.ResourceGroupAddResourcesResponse{Status: s.okStatus()}, nil
}

func (s *testDPSServer) ResourceGroupRemoveResources(_ context.Context, request *dpsv1.ResourceGroupRemoveResourcesRequest) (*dpsv1.ResourceGroupRemoveResourcesResponse, error) {
	s.removeRequest = request
	return &dpsv1.ResourceGroupRemoveResourcesResponse{Status: s.okStatus()}, nil
}

func (s *testDPSServer) ActivateResourceGroup(_ context.Context, request *dpsv1.ActivateResourceGroupRequest) (*dpsv1.ActivateResourceGroupResponse, error) {
	s.activateRequest = request
	s.activateGroup = request.GetGroupName()
	if s.activateError != nil {
		return nil, s.activateError
	}
	return &dpsv1.ActivateResourceGroupResponse{Status: s.okStatus()}, nil
}

func (s *testDPSServer) ResourceGroupUpdateResources(_ context.Context, request *dpsv1.ResourceGroupUpdateResourcesRequest) (*dpsv1.ResourceGroupUpdateResourcesResponse, error) {
	s.updateRequest = request
	return &dpsv1.ResourceGroupUpdateResourcesResponse{Status: s.okStatus()}, nil
}

func (s *testDPSServer) okStatus() *dpsv1.Status {
	if s.responseStatus != nil {
		return s.responseStatus
	}
	return &dpsv1.Status{Ok: true}
}

func newTestClient(t *testing.T, server *testDPSServer) *Client {
	t.Helper()

	listener := bufconn.Listen(1024 * 1024)
	grpcServer := grpc.NewServer()
	dpsv1.RegisterTopologyManagementServiceServer(grpcServer, server)
	dpsv1.RegisterResourceGroupManagementServiceServer(grpcServer, server)
	go func() {
		_ = grpcServer.Serve(listener)
	}()
	t.Cleanup(grpcServer.Stop)

	connection, err := grpc.NewClient(
		"passthrough:///dps-test",
		grpc.WithContextDialer(func(context.Context, string) (net.Conn, error) {
			return listener.Dial()
		}),
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, connection.Close()) })

	client := newClient(connection, time.Second)
	client.retryWait = 0
	return client
}

func TestClient_ListPowerProfiles(t *testing.T) {
	tests := []struct {
		name          string
		server        *testDPSServer
		expected      []string
		expectedCalls int
	}{
		{
			name:          "normalizes policy names",
			server:        &testDPSServer{policies: []string{" balanced ", "performance", "", "balanced"}},
			expected:      []string{"balanced", "performance"},
			expectedCalls: 1,
		},
		{
			name:          "retries unavailable once",
			server:        &testDPSServer{policies: []string{"performance"}, policyErrors: []error{status.Error(codes.Unavailable, "retry")}},
			expected:      []string{"performance"},
			expectedCalls: 2,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			client := newTestClient(t, test.server)
			profiles, err := client.ListPowerProfiles(context.Background())
			require.NoError(t, err)
			assert.Equal(t, test.expected, profiles)
			assert.Equal(t, test.expectedCalls, test.server.policyCalls)
		})
	}
}

func TestClient_CreateResourceGroup(t *testing.T) {
	tests := []struct {
		name           string
		server         *testDPSServer
		expectedError  string
		expectedTarget error
	}{
		{name: "creates resource group", server: &testDPSServer{}},
		{
			name:          "reports response status failure",
			server:        &testDPSServer{responseStatus: &dpsv1.Status{DiagMsg: "topology is inactive"}},
			expectedError: "topology is inactive",
		},
		{
			name:           "classifies gRPC name collision",
			server:         &testDPSServer{createError: status.Error(codes.AlreadyExists, "resource group already exists")},
			expectedError:  "resource group already exists",
			expectedTarget: ErrResourceGroupAlreadyExists,
		},
		{
			name:           "classifies response status name collision",
			server:         &testDPSServer{responseStatus: &dpsv1.Status{DiagMsg: "resource group already exists"}},
			expectedError:  "resource group already exists",
			expectedTarget: ErrResourceGroupAlreadyExists,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			client := newTestClient(t, test.server)
			err := client.CreateResourceGroup(context.Background(), " group-a ", 42)
			if test.expectedError != "" {
				require.ErrorContains(t, err, test.expectedError)
			} else {
				require.NoError(t, err)
			}
			if test.expectedTarget != nil {
				require.True(t, errors.Is(err, test.expectedTarget))
			}

			require.NotNil(t, test.server.createRequest)
			assert.Equal(t, "group-a", test.server.createRequest.GetGroupName())
			assert.EqualValues(t, 42, test.server.createRequest.GetExternalId())
			assert.True(t, test.server.createRequest.GetDpmEnable())
			assert.True(t, test.server.createRequest.GetPrsEnabled())
			assert.True(t, test.server.createRequest.GetSharedGpuEnable())
		})
	}
}

func TestClient_ActivateResourceGroup(t *testing.T) {
	tests := []struct {
		name   string
		server *testDPSServer
	}{
		{name: "activates resource group", server: &testDPSServer{}},
		{
			name:   "accepts already active transport status",
			server: &testDPSServer{activateError: status.Error(codes.FailedPrecondition, "already active")},
		},
		{
			name:   "accepts already active response status",
			server: &testDPSServer{responseStatus: &dpsv1.Status{DiagMsg: "resource group already active"}},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			client := newTestClient(t, test.server)
			require.NoError(t, client.ActivateResourceGroup(context.Background(), "group-a"))

			assert.Equal(t, "group-a", test.server.activateGroup)
			require.NotNil(t, test.server.activateRequest)
			assert.True(t, test.server.activateRequest.GetStrict())
			assert.False(t, test.server.activateRequest.GetAllowReprovision())
			assert.Nil(t, test.server.activateRequest.GetAsync())
		})
	}
}

func TestClient_DeleteResourceGroup(t *testing.T) {
	tests := []struct {
		name   string
		server *testDPSServer
	}{
		{name: "deletes resource group", server: &testDPSServer{}},
		{
			name:   "accepts missing resource group",
			server: &testDPSServer{deleteError: status.Error(codes.NotFound, "missing")},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			client := newTestClient(t, test.server)
			require.NoError(t, client.DeleteResourceGroup(context.Background(), "group-a"))

			assert.Equal(t, "group-a", test.server.deleteGroup)
			require.NotNil(t, test.server.deleteRequest)
			assert.True(t, test.server.deleteRequest.GetWppsDisableAsyncVerification())
		})
	}
}

func TestClient_AddMachines(t *testing.T) {
	tests := []struct {
		name          string
		resourceGroup string
		machineIDs    []string
		powerProfile  string
		expectedError string
	}{
		{name: "adds batch", resourceGroup: "group-a", machineIDs: []string{" machine-a ", "machine-b"}, powerProfile: " performance "},
		{name: "rejects empty batch", resourceGroup: "group-a", expectedError: "machine IDs are required"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server := &testDPSServer{}
			client := newTestClient(t, server)
			err := client.AddMachines(context.Background(), test.resourceGroup, test.machineIDs, test.powerProfile)
			if test.expectedError != "" {
				require.ErrorContains(t, err, test.expectedError)
				assert.Nil(t, server.addRequest)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, server.addRequest)
			assert.Equal(t, []string{"machine-a", "machine-b"}, server.addRequest.GetResourceNames())
			assert.True(t, server.addRequest.GetStrict())
			assert.False(t, server.addRequest.GetAllowReprovision())
			require.NotNil(t, server.addRequest.PolicyName)
			assert.Equal(t, "performance", server.addRequest.GetPolicyName())
		})
	}
}

func TestClient_UpdateMachineProfile(t *testing.T) {
	tests := []struct {
		name          string
		machineID     string
		powerProfile  string
		expectedError string
	}{
		{name: "clears profile", machineID: "machine-a"},
		{name: "rejects empty machine ID", machineID: " ", expectedError: "machine ID is required"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server := &testDPSServer{}
			client := newTestClient(t, server)
			err := client.UpdateMachineProfile(context.Background(), "group-a", test.machineID, test.powerProfile)
			if test.expectedError != "" {
				require.ErrorContains(t, err, test.expectedError)
				assert.Nil(t, server.updateRequest)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, server.updateRequest)
			assert.Nil(t, server.updateRequest.GetAsync())
			require.Len(t, server.updateRequest.GetUpdates(), 1)
			update := server.updateRequest.GetUpdates()[0]
			assert.Equal(t, "machine-a", update.GetResourceName())
			require.IsType(t, &dpsv1.ResourceGroupUpdateResourcesRequest_ResourcePolicy_PolicyName{}, update.GetPolicyUpdate())
			assert.Equal(t, "", update.GetPolicyName())
		})
	}
}

func TestClient_RemoveMachines(t *testing.T) {
	tests := []struct {
		name          string
		machineIDs    []string
		expectedError string
	}{
		{name: "removes batch", machineIDs: []string{" machine-a ", "machine-b"}},
		{name: "rejects empty batch", expectedError: "machine IDs are required"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server := &testDPSServer{}
			client := newTestClient(t, server)
			err := client.RemoveMachines(context.Background(), "group-a", test.machineIDs)
			if test.expectedError != "" {
				require.ErrorContains(t, err, test.expectedError)
				assert.Nil(t, server.removeRequest)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, server.removeRequest)
			assert.Equal(t, []string{"machine-a", "machine-b"}, server.removeRequest.GetResourceNames())
		})
	}
}

func TestClient_ValidateAllocation(t *testing.T) {
	tests := []struct {
		name          string
		server        *testDPSServer
		expectedError error
		expectedCalls int
	}{
		{
			name: "accepts allocation",
			server: &testDPSServer{validationResult: &dpsv1.ValidateAllocationResponse_AllocationValidationResult{
				Status:                 &dpsv1.Status{Ok: true},
				AllocationWouldSucceed: true,
			}},
			expectedCalls: 1,
		},
		{
			name: "rejects denied allocation",
			server: &testDPSServer{validationResult: &dpsv1.ValidateAllocationResponse_AllocationValidationResult{
				Status: &dpsv1.Status{Ok: true},
			}},
			expectedError: errAllocationRejected,
			expectedCalls: 1,
		},
		{
			name: "retries unavailable once",
			server: &testDPSServer{
				validateErrors: []error{status.Error(codes.Unavailable, "retry")},
				validationResult: &dpsv1.ValidateAllocationResponse_AllocationValidationResult{
					Status:                 &dpsv1.Status{Ok: true},
					AllocationWouldSucceed: true,
				},
			},
			expectedCalls: 2,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			client := newTestClient(t, test.server)
			err := client.ValidateAllocation(context.Background(), []string{" machine-b ", "machine-a", "machine-a"}, " performance ")
			if test.expectedError == nil {
				require.NoError(t, err)
			} else {
				require.ErrorIs(t, err, test.expectedError)
			}
			assert.Equal(t, test.expectedCalls, test.server.validateCalls)
			require.NotNil(t, test.server.validateRequest)
			assert.Equal(t, []string{"machine-a", "machine-b"}, test.server.validateRequest.GetDeviceNames())
			assert.Equal(t, "performance", test.server.validateRequest.GetPolicyName())
			assert.True(t, test.server.validateRequest.GetStrict())
		})
	}
}

func TestClient_ResponseStatusMissingFailsClosed(t *testing.T) {
	err := responseStatus("ValidateAllocation", nil)
	require.ErrorContains(t, err, "missing operation status")
}

func TestClient_RejectsEmptyAssociationNames(t *testing.T) {
	client := newTestClient(t, &testDPSServer{})

	tests := []struct {
		name string
		call func() error
	}{
		{name: "resource group", call: func() error {
			return client.AddMachine(context.Background(), " ", "machine-a", "")
		}},
		{name: "machine ID", call: func() error {
			return client.AddMachine(context.Background(), "group-a", " ", "")
		}},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			require.ErrorContains(t, test.call(), "required")
		})
	}
}
