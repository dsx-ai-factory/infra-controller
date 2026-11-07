// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dps

import (
	"context"
	"errors"
	"fmt"
	"io"
	"slices"
	"strings"
	"time"

	dpsv1 "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps/internal/dpssdk/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
)

const (
	readOnlyAttempts        = 2
	readOnlyRetryWait       = time.Second
	alreadyActiveDiagnostic = "already active"
	alreadyExistsDiagnostic = "already exists"
)

var errAllocationRejected = errors.New("DPS allocation would not succeed")

var _ PowerProvisioner = (*Client)(nil)

// Client implements the narrow power-provisioning contract with direct DPS
// gRPC calls.
type Client struct {
	connection     *grpc.ClientConn
	requestTimeout time.Duration
	policies       dpsv1.TopologyManagementServiceClient
	resourceGroups dpsv1.ResourceGroupManagementServiceClient
	retryWait      time.Duration
}

// NewClient creates a reusable authenticated DPS client.
func NewClient(config Config) (*Client, error) {
	connection, err := NewConnection(config)
	if err != nil {
		return nil, err
	}

	return newClient(connection, config.RequestTimeout), nil
}

func newClient(connection *grpc.ClientConn, requestTimeout time.Duration) *Client {
	return &Client{
		connection:     connection,
		requestTimeout: requestTimeout,
		policies:       dpsv1.NewTopologyManagementServiceClient(connection),
		resourceGroups: dpsv1.NewResourceGroupManagementServiceClient(connection),
		retryWait:      readOnlyRetryWait,
	}
}

// Close releases the underlying DPS connection.
func (c *Client) Close() error {
	if c == nil || c.connection == nil {
		return nil
	}
	return c.connection.Close()
}

func (c *Client) requestContext(ctx context.Context) (context.Context, context.CancelFunc) {
	return context.WithTimeout(ctx, c.requestTimeout)
}

// ListPowerProfiles returns the normalized policy names reported by DPS.
func (c *Client) ListPowerProfiles(ctx context.Context) ([]string, error) {
	requestCtx, cancel := c.requestContext(ctx)
	defer cancel()

	var profiles map[string]struct{}
	err := c.withReadOnlyRetry(requestCtx, func() error {
		stream, err := c.policies.ListPolicies(requestCtx, &emptypb.Empty{})
		if err != nil {
			return fmt.Errorf("DPS ListPolicies: %w", err)
		}

		attemptProfiles := make(map[string]struct{})
		for {
			policy, recvErr := stream.Recv()
			if errors.Is(recvErr, io.EOF) {
				profiles = attemptProfiles
				return nil
			}
			if recvErr != nil {
				return fmt.Errorf("receive DPS ListPolicies response: %w", recvErr)
			}
			if policy == nil {
				continue
			}

			name := strings.TrimSpace(policy.GetName())
			if name != "" {
				attemptProfiles[name] = struct{}{}
			}
		}
	})
	if err != nil {
		return nil, err
	}

	names := make([]string, 0, len(profiles))
	for name := range profiles {
		names = append(names, name)
	}
	slices.Sort(names)
	return names, nil
}

// ValidateAllocation asks DPS whether the requested machines can receive the
// exact power profile without changing DPS state.
func (c *Client) ValidateAllocation(ctx context.Context, machineIDs []string, powerProfile string) error {
	powerProfile, err := requiredName("power profile", powerProfile)
	if err != nil {
		return err
	}

	normalizedIDs := make(map[string]struct{}, len(machineIDs))
	for _, machineID := range machineIDs {
		machineID, err = requiredName("machine ID", machineID)
		if err != nil {
			return err
		}
		normalizedIDs[machineID] = struct{}{}
	}
	if len(normalizedIDs) == 0 {
		return fmt.Errorf("DPS machine ID is required")
	}
	machineIDs = make([]string, 0, len(normalizedIDs))
	for machineID := range normalizedIDs {
		machineIDs = append(machineIDs, machineID)
	}
	slices.Sort(machineIDs)

	requestCtx, cancel := c.requestContext(ctx)
	defer cancel()

	return c.withReadOnlyRetry(requestCtx, func() error {
		stream, streamErr := c.policies.ValidateAllocation(requestCtx, &dpsv1.ValidateAllocationRequest{
			DeviceNames: machineIDs,
			PolicyName:  powerProfile,
			Strict:      true,
		})
		if streamErr != nil {
			return fmt.Errorf("DPS ValidateAllocation: %w", streamErr)
		}

		foundResult := false
		for {
			response, recvErr := stream.Recv()
			if errors.Is(recvErr, io.EOF) {
				if !foundResult {
					return fmt.Errorf("DPS ValidateAllocation returned no allocation result")
				}
				return nil
			}
			if recvErr != nil {
				return fmt.Errorf("receive DPS ValidateAllocation response: %w", recvErr)
			}
			result := response.GetAllocationValidationResult()
			if result == nil {
				continue
			}
			foundResult = true
			statusErr := responseStatus("ValidateAllocation", result.GetStatus())
			if statusErr != nil {
				return statusErr
			}
			if !result.GetAllocationWouldSucceed() {
				return errAllocationRejected
			}
		}
	})
}

// CreateResourceGroup creates an empty, inactive DPS resource group with the
// required Max-Q options for DPS 0.8.
func (c *Client) CreateResourceGroup(ctx context.Context, resourceGroup string, externalID int64) error {
	resourceGroup, err := requiredName("resource group", resourceGroup)
	if err != nil {
		return err
	}

	requestCtx, cancel := c.requestContext(ctx)
	defer cancel()

	prsEnabled := true
	response, err := c.resourceGroups.ResourceGroupCreate(requestCtx, &dpsv1.ResourceGroupCreateRequest{
		ExternalId:      externalID,
		GroupName:       resourceGroup,
		PrsEnabled:      &prsEnabled,
		DpmEnable:       true,
		SharedGpuEnable: true,
	})
	if status.Code(err) == codes.AlreadyExists {
		return fmt.Errorf("%w: %v", ErrResourceGroupAlreadyExists, err)
	}
	if err != nil {
		return fmt.Errorf("DPS ResourceGroupCreate %q: %w", resourceGroup, err)
	}
	if response != nil && !response.GetStatus().GetOk() && strings.Contains(strings.ToLower(response.GetStatus().GetDiagMsg()), alreadyExistsDiagnostic) {
		return fmt.Errorf("%w: %s", ErrResourceGroupAlreadyExists, response.GetStatus().GetDiagMsg())
	}
	return responseStatus("ResourceGroupCreate", response.GetStatus())
}

// DeleteResourceGroup deletes a DPS resource group. An already-absent group is
// considered successfully deleted.
func (c *Client) DeleteResourceGroup(ctx context.Context, resourceGroup string) error {
	resourceGroup, err := requiredName("resource group", resourceGroup)
	if err != nil {
		return err
	}

	requestCtx, cancel := c.requestContext(ctx)
	defer cancel()

	response, err := c.resourceGroups.ResourceGroupDelete(requestCtx, &dpsv1.ResourceGroupDeleteRequest{
		GroupName:                    resourceGroup,
		WppsDisableAsyncVerification: true,
	})
	if status.Code(err) == codes.NotFound {
		return nil
	}
	if err != nil {
		return fmt.Errorf("DPS ResourceGroupDelete %q: %w", resourceGroup, err)
	}
	return responseStatus("ResourceGroupDelete", response.GetStatus())
}

// AddMachine adds one NICo machine to a DPS resource group. An empty profile
// omits the entity-level policy so DPS uses the group or topology policy.
func (c *Client) AddMachine(ctx context.Context, resourceGroup, machineID, powerProfile string) error {
	return c.AddMachines(ctx, resourceGroup, []string{machineID}, powerProfile)
}

// AddMachines adds NICo machines to a DPS resource group in one request. An
// empty profile omits the entity-level policy so DPS uses the group or topology
// policy.
func (c *Client) AddMachines(ctx context.Context, resourceGroup string, machineIDs []string, powerProfile string) error {
	resourceGroup, machineIDs, err := requiredAssociationNames(resourceGroup, machineIDs)
	if err != nil {
		return err
	}

	request := &dpsv1.ResourceGroupAddResourcesRequest{
		GroupName:        resourceGroup,
		ResourceNames:    machineIDs,
		Strict:           true,
		AllowReprovision: false,
	}
	powerProfile = strings.TrimSpace(powerProfile)
	if powerProfile != "" {
		request.PolicyName = &powerProfile
	}

	requestCtx, cancel := c.requestContext(ctx)
	defer cancel()
	response, err := c.resourceGroups.ResourceGroupAddResources(requestCtx, request)
	if err != nil {
		return fmt.Errorf("DPS ResourceGroupAddResources %q: %w", resourceGroup, err)
	}
	return responseStatus("ResourceGroupAddResources", response.GetStatus())
}

// UpdateMachineProfile sets or clears one machine's entity-level policy. The
// policy_name oneof is always present, including when its value is empty.
func (c *Client) UpdateMachineProfile(ctx context.Context, resourceGroup, machineID, powerProfile string) error {
	resourceGroup, machineIDs, err := requiredAssociationNames(resourceGroup, []string{machineID})
	if err != nil {
		return err
	}
	machineID = machineIDs[0]
	powerProfile = strings.TrimSpace(powerProfile)

	requestCtx, cancel := c.requestContext(ctx)
	defer cancel()
	response, err := c.resourceGroups.ResourceGroupUpdateResources(requestCtx, &dpsv1.ResourceGroupUpdateResourcesRequest{
		GroupName: resourceGroup,
		Updates: []*dpsv1.ResourceGroupUpdateResourcesRequest_ResourcePolicy{
			{
				ResourceName: machineID,
				PolicyUpdate: &dpsv1.ResourceGroupUpdateResourcesRequest_ResourcePolicy_PolicyName{
					PolicyName: powerProfile,
				},
			},
		},
	})
	if err != nil {
		return fmt.Errorf("DPS ResourceGroupUpdateResources %q: %w", resourceGroup, err)
	}
	return responseStatus("ResourceGroupUpdateResources", response.GetStatus())
}

// RemoveMachine removes one NICo machine from a DPS resource group.
func (c *Client) RemoveMachine(ctx context.Context, resourceGroup, machineID string) error {
	return c.RemoveMachines(ctx, resourceGroup, []string{machineID})
}

// RemoveMachines removes NICo machines from a DPS resource group in one
// request.
func (c *Client) RemoveMachines(ctx context.Context, resourceGroup string, machineIDs []string) error {
	resourceGroup, machineIDs, err := requiredAssociationNames(resourceGroup, machineIDs)
	if err != nil {
		return err
	}

	requestCtx, cancel := c.requestContext(ctx)
	defer cancel()
	response, err := c.resourceGroups.ResourceGroupRemoveResources(requestCtx, &dpsv1.ResourceGroupRemoveResourcesRequest{
		GroupName:     resourceGroup,
		ResourceNames: machineIDs,
	})
	if err != nil {
		return fmt.Errorf("DPS ResourceGroupRemoveResources %q: %w", resourceGroup, err)
	}
	return responseStatus("ResourceGroupRemoveResources", response.GetStatus())
}

// ActivateResourceGroup activates a populated DPS resource group. DPS reports
// an already-active group as FailedPrecondition, which is treated as success.
func (c *Client) ActivateResourceGroup(ctx context.Context, resourceGroup string) error {
	resourceGroup, err := requiredName("resource group", resourceGroup)
	if err != nil {
		return err
	}

	requestCtx, cancel := c.requestContext(ctx)
	defer cancel()
	response, err := c.resourceGroups.ActivateResourceGroup(requestCtx, &dpsv1.ActivateResourceGroupRequest{
		GroupName:        resourceGroup,
		Strict:           true,
		AllowReprovision: false,
	})
	if isAlreadyActive(err) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("DPS ActivateResourceGroup %q: %w", resourceGroup, err)
	}
	responseErr := responseStatus("ActivateResourceGroup", response.GetStatus())
	if responseErr != nil {
		// DPS 0.8 reports idempotent activation through this diagnostic text
		// instead of a structured status code.
		if strings.Contains(strings.ToLower(responseErr.Error()), alreadyActiveDiagnostic) {
			return nil
		}
		return responseErr
	}
	return nil
}

func requiredAssociationNames(resourceGroup string, machineIDs []string) (string, []string, error) {
	resourceGroup, err := requiredName("resource group", resourceGroup)
	if err != nil {
		return "", nil, err
	}
	if len(machineIDs) == 0 {
		return "", nil, fmt.Errorf("machine IDs are required")
	}
	normalizedMachineIDs := make([]string, 0, len(machineIDs))
	for _, machineID := range machineIDs {
		machineID, err = requiredName("machine ID", machineID)
		if err != nil {
			return "", nil, err
		}
		normalizedMachineIDs = append(normalizedMachineIDs, machineID)
	}
	return resourceGroup, normalizedMachineIDs, nil
}

func requiredName(field, value string) (string, error) {
	value = strings.TrimSpace(value)
	if value == "" {
		return "", fmt.Errorf("DPS %s is required", field)
	}
	return value, nil
}

func responseStatus(operation string, operationStatus *dpsv1.Status) error {
	if operationStatus == nil {
		return fmt.Errorf("DPS %s: response is missing operation status", operation)
	}
	if operationStatus.GetOk() {
		return nil
	}
	diagnostic := strings.TrimSpace(operationStatus.GetDiagMsg())
	if diagnostic == "" {
		diagnostic = "status not ok"
	}
	return fmt.Errorf("DPS %s: %s", operation, diagnostic)
}

func isAlreadyActive(err error) bool {
	return status.Code(err) == codes.FailedPrecondition &&
		strings.Contains(strings.ToLower(err.Error()), "already active")
}

func (c *Client) withReadOnlyRetry(ctx context.Context, operation func() error) error {
	var err error
	for attempt := range readOnlyAttempts {
		err = operation()
		if err == nil || status.Code(err) != codes.Unavailable || attempt == readOnlyAttempts-1 {
			return err
		}

		timer := time.NewTimer(c.retryWait)
		select {
		case <-ctx.Done():
			if !timer.Stop() {
				<-timer.C
			}
			return ctx.Err()
		case <-timer.C:
		}
	}
	return err
}
