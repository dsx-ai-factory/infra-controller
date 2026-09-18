// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"

	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cloudutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
)

type UpdateInstanceTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateInstanceTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateInstanceTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateInstanceTestSuite) Test_UpdateInstance_Success() {
	var machineManager iActivity.ManageInstance

	ipxeScript := "#!ipxe"
	userData := "echo"
	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.InstanceConfigUpdateRequest{
		InstanceId: &corev1.InstanceId{Value: uuid.NewString()},
		Metadata: &corev1.Metadata{
			Name:        "updated_name",
			Description: "updated_description",
			Labels: []*corev1.Label{
				{
					Key:   labelKey,
					Value: &labelValue,
				},
			},
		},
		Config: &corev1.InstanceConfig{
			Os: &corev1.InstanceOperatingSystemConfig{
				RunProvisioningInstructionsOnEveryBoot: true,
				Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
					Ipxe: &corev1.InlineIpxe{
						IpxeScript: ipxeScript,
					},
				},
				UserData: &userData,
			},
		},
	}

	// Mock UpdateInstanceOnSiteActivity activity
	s.env.RegisterActivity(machineManager.UpdateInstanceOnSite)
	s.env.OnActivity(machineManager.UpdateInstanceOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateMachineInventory workflow
	s.env.ExecuteWorkflow(UpdateInstance, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateInstanceTestSuite) Test_UpdateInstance_Failure() {
	var machineManager iActivity.ManageInstance

	ipxeScript := "#!ipxe"
	userData := "echo"
	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.InstanceConfigUpdateRequest{
		InstanceId: &corev1.InstanceId{Value: uuid.NewString()},
		Metadata: &corev1.Metadata{
			Name:        "updated_name",
			Description: "updated_description",
			Labels: []*corev1.Label{
				{
					Key:   labelKey,
					Value: &labelValue,
				},
			},
		},
		Config: &corev1.InstanceConfig{
			Os: &corev1.InstanceOperatingSystemConfig{
				RunProvisioningInstructionsOnEveryBoot: true,
				Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
					Ipxe: &corev1.InlineIpxe{
						IpxeScript: ipxeScript,
					},
				},
				UserData: &userData,
			},
		},
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateInstanceOnSiteActivity activity
	s.env.RegisterActivity(machineManager.UpdateInstanceOnSite)
	s.env.OnActivity(machineManager.UpdateInstanceOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateMachineInventory workflow
	s.env.ExecuteWorkflow(UpdateInstance, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestUpdateInstanceTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateInstanceTestSuite))
}

type CreateInstanceV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *CreateInstanceV2TestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *CreateInstanceV2TestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *CreateInstanceV2TestSuite) Test_CreateInstanceV2_Success() {
	var machineManager iActivity.ManageInstance

	ipxeScript := "#!ipxe"
	userData := "echo"
	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.InstanceAllocationRequest{
		MachineId: &corev1.MachineId{Id: uuid.NewString()},
		Metadata: &corev1.Metadata{
			Name:        "updated_name",
			Description: "updated_description",
			Labels: []*corev1.Label{
				{
					Key:   labelKey,
					Value: &labelValue,
				},
			},
		},
		Config: &corev1.InstanceConfig{
			Os: &corev1.InstanceOperatingSystemConfig{
				RunProvisioningInstructionsOnEveryBoot: true,
				Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
					Ipxe: &corev1.InlineIpxe{
						IpxeScript: ipxeScript,
					},
				},
				UserData: &userData,
			},
		},
	}

	// Mock CreateInstanceOnSiteActivity activity
	s.env.RegisterActivity(machineManager.CreateInstanceOnSite)
	s.env.OnActivity(machineManager.CreateInstanceOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(CreateInstanceV2, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *CreateInstanceV2TestSuite) Test_CreateInstanceV2_Failure() {
	var machineManager iActivity.ManageInstance

	ipxeScript := "#!ipxe"
	userData := "echo"
	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.InstanceAllocationRequest{
		MachineId: &corev1.MachineId{Id: uuid.NewString()},
		Metadata: &corev1.Metadata{
			Name:        "updated_name",
			Description: "updated_description",
			Labels: []*corev1.Label{
				{
					Key:   labelKey,
					Value: &labelValue,
				},
			},
		},
		Config: &corev1.InstanceConfig{
			Os: &corev1.InstanceOperatingSystemConfig{
				RunProvisioningInstructionsOnEveryBoot: true,
				Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
					Ipxe: &corev1.InlineIpxe{
						IpxeScript: ipxeScript,
					},
				},
				UserData: &userData,
			},
		},
	}

	errMsg := "Site Controller communication error"

	// Mock CreateInstanceOnSiteActivity activity
	s.env.RegisterActivity(machineManager.CreateInstanceOnSite)
	s.env.OnActivity(machineManager.CreateInstanceOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute CreateMachineInventory workflow
	s.env.ExecuteWorkflow(CreateInstanceV2, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

// Test_CreateInstanceV2_ActivityDeadlineTracksLadder pins the budget that bounds
// Core. CreateInstanceOnSite passes this context to AllocateInstance, so Core's own
// deadline is whatever the activity carries. A budget outliving the REST caller's
// wait would let Core commit an Instance the handler has already rolled back.
func (s *CreateInstanceV2TestSuite) Test_CreateInstanceV2_ActivityDeadlineTracksLadder() {
	var machineManager iActivity.ManageInstance

	var deadline time.Time
	var hasDeadline bool

	s.env.RegisterActivity(machineManager.CreateInstanceOnSite)
	s.env.OnActivity(machineManager.CreateInstanceOnSite, mock.Anything, mock.Anything).
		Run(func(args mock.Arguments) {
			deadline, hasDeadline = args.Get(0).(context.Context).Deadline()
		}).Return(nil)

	s.env.ExecuteWorkflow(CreateInstanceV2, &corev1.InstanceAllocationRequest{
		MachineId: &corev1.MachineId{Id: uuid.NewString()},
	})

	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
	s.True(hasDeadline)

	// Assert against the constant, not a literal, so retuning the ladder does not
	// require editing this test.
	remaining := time.Until(deadline)
	s.Greater(remaining, cloudutils.ActivityStartToCloseTimeout-time.Second)
	s.LessOrEqual(remaining, cloudutils.ActivityStartToCloseTimeout)
}

// Test_CreateInstanceV2_DoesNotRetry pins the single attempt. Temporal anchors
// StartToCloseTimeout at the moment an attempt starts, so a second attempt gets a
// fresh budget that can outlive both the workflow and the caller, letting Core
// commit an Instance the handler has already rolled back.
func (s *CreateInstanceV2TestSuite) Test_CreateInstanceV2_DoesNotRetry() {
	var machineManager iActivity.ManageInstance

	attempts := 0

	s.env.RegisterActivity(machineManager.CreateInstanceOnSite)
	s.env.OnActivity(machineManager.CreateInstanceOnSite, mock.Anything, mock.Anything).
		Run(func(mock.Arguments) {
			attempts++
		}).Return(errors.New("Site Controller communication error"))

	s.env.ExecuteWorkflow(CreateInstanceV2, &corev1.InstanceAllocationRequest{
		MachineId: &corev1.MachineId{Id: uuid.NewString()},
	})

	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
	s.Equal(1, attempts)
}

func TestCreateInstanceV2TestSuite(t *testing.T) {
	suite.Run(t, new(CreateInstanceV2TestSuite))
}

// CreateInstancesTestSuite tests the CreateInstances workflow which handles
// batch instance allocation requests. This is the multi-instance version of CreateInstanceV2.
type CreateInstancesTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *CreateInstancesTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *CreateInstancesTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

// Test_CreateInstances_Success verifies that the workflow completes successfully
// when the CreateInstancesOnSite activity returns no error for a batch of 2 instances.
func (s *CreateInstancesTestSuite) Test_CreateInstances_Success() {
	var instanceManager iActivity.ManageInstance

	ipxeScript := "#!ipxe"
	userData := "echo"
	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.BatchInstanceAllocationRequest{
		InstanceRequests: []*corev1.InstanceAllocationRequest{
			{
				MachineId: &corev1.MachineId{Id: uuid.NewString()},
				Metadata: &corev1.Metadata{
					Name:        "instance_1",
					Description: "first instance",
					Labels: []*corev1.Label{
						{
							Key:   labelKey,
							Value: &labelValue,
						},
					},
				},
				Config: &corev1.InstanceConfig{
					Os: &corev1.InstanceOperatingSystemConfig{
						RunProvisioningInstructionsOnEveryBoot: true,
						Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
							Ipxe: &corev1.InlineIpxe{
								IpxeScript: ipxeScript,
							},
						},
						UserData: &userData,
					},
				},
			},
			{
				MachineId: &corev1.MachineId{Id: uuid.NewString()},
				Metadata: &corev1.Metadata{
					Name:        "instance_2",
					Description: "second instance",
					Labels: []*corev1.Label{
						{
							Key:   labelKey,
							Value: &labelValue,
						},
					},
				},
				Config: &corev1.InstanceConfig{
					Os: &corev1.InstanceOperatingSystemConfig{
						RunProvisioningInstructionsOnEveryBoot: true,
						Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
							Ipxe: &corev1.InlineIpxe{
								IpxeScript: ipxeScript,
							},
						},
						UserData: &userData,
					},
				},
			},
		},
	}

	// Mock CreateInstancesOnSite activity
	s.env.RegisterActivity(instanceManager.CreateInstancesOnSite)
	s.env.OnActivity(instanceManager.CreateInstancesOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(CreateInstances, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

// Test_CreateInstances_Failure verifies that the workflow returns an error
// when the CreateInstancesOnSite activity fails (e.g., Site Controller communication error).
func (s *CreateInstancesTestSuite) Test_CreateInstances_Failure() {
	var instanceManager iActivity.ManageInstance

	ipxeScript := "#!ipxe"
	userData := "echo"
	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.BatchInstanceAllocationRequest{
		InstanceRequests: []*corev1.InstanceAllocationRequest{
			{
				MachineId: &corev1.MachineId{Id: uuid.NewString()},
				Metadata: &corev1.Metadata{
					Name:        "instance_1",
					Description: "first instance",
					Labels: []*corev1.Label{
						{
							Key:   labelKey,
							Value: &labelValue,
						},
					},
				},
				Config: &corev1.InstanceConfig{
					Os: &corev1.InstanceOperatingSystemConfig{
						RunProvisioningInstructionsOnEveryBoot: true,
						Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
							Ipxe: &corev1.InlineIpxe{
								IpxeScript: ipxeScript,
							},
						},
						UserData: &userData,
					},
				},
			},
		},
	}

	errMsg := "Site Controller communication error"

	// Mock CreateInstancesOnSite activity
	s.env.RegisterActivity(instanceManager.CreateInstancesOnSite)
	s.env.OnActivity(instanceManager.CreateInstancesOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute workflow
	s.env.ExecuteWorkflow(CreateInstances, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestCreateInstancesTestSuite(t *testing.T) {
	suite.Run(t, new(CreateInstancesTestSuite))
}

type DeleteInstanceV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteInstanceV2TestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteInstanceV2TestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteInstanceV2TestSuite) Test_DeleteInstanceV2_Success() {
	var instanceManager iActivity.ManageInstance

	request := &corev1.InstanceReleaseRequest{
		Id: &corev1.InstanceId{Value: uuid.NewString()},
	}

	// Mock DeleteInstanceOnSiteActivity activity
	s.env.RegisterActivity(instanceManager.DeleteInstanceOnSite)
	s.env.OnActivity(instanceManager.DeleteInstanceOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DeleteInstanceV2, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteInstanceV2TestSuite) Test_DeleteInstanceV2_Failure() {
	var machineManager iActivity.ManageInstance

	request := &corev1.InstanceReleaseRequest{
		Id: &corev1.InstanceId{Value: uuid.NewString()},
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteInstanceOnSiteActivity activity
	s.env.RegisterActivity(machineManager.DeleteInstanceOnSite)
	s.env.OnActivity(machineManager.DeleteInstanceOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteMachineInventory workflow
	s.env.ExecuteWorkflow(DeleteInstanceV2, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestDeleteInstanceV2TestSuite(t *testing.T) {
	suite.Run(t, new(DeleteInstanceV2TestSuite))
}

type RebootInstanceTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *RebootInstanceTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *RebootInstanceTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *RebootInstanceTestSuite) Test_RebootInstance_Success() {
	var machineManager iActivity.ManageInstance

	request := &corev1.InstancePowerRequest{
		InstanceId: &corev1.InstanceId{Value: uuid.NewString()},
		Operation:  corev1.InstancePowerRequest_POWER_RESET,
	}

	// Mock RebootInstanceOnSiteActivity activity
	s.env.RegisterActivity(machineManager.RebootInstanceOnSite)
	s.env.OnActivity(machineManager.RebootInstanceOnSite, mock.Anything, request).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(RebootInstance, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *RebootInstanceTestSuite) Test_RebootInstance_Failure() {
	var machineManager iActivity.ManageInstance

	request := &corev1.InstancePowerRequest{
		InstanceId: &corev1.InstanceId{Value: uuid.NewString()},
		Operation:  corev1.InstancePowerRequest_POWER_RESET,
	}

	errMsg := "Site Controller communication error"

	// Mock RebootInstanceOnSiteActivity activity
	s.env.RegisterActivity(machineManager.RebootInstanceOnSite)
	s.env.OnActivity(machineManager.RebootInstanceOnSite, mock.Anything, request).Return(errors.New(errMsg))

	// execute RebootMachineInventory workflow
	s.env.ExecuteWorkflow(RebootInstance, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestRebootInstanceTestSuite(t *testing.T) {
	suite.Run(t, new(RebootInstanceTestSuite))
}

type InventoryInstanceTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventoryInstanceTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventoryInstanceTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventoryInstanceTestSuite) Test_DiscoverInstanceInventory_Success() {
	var inventoryManager iActivity.ManageInstanceInventory

	s.env.RegisterActivity(inventoryManager.DiscoverInstanceInventory)
	s.env.OnActivity(inventoryManager.DiscoverInstanceInventory, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DiscoverInstanceInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *InventoryInstanceTestSuite) Test_DiscoverInstanceInventory_ActivityFails() {
	var inventoryManager iActivity.ManageInstanceInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverInstanceInventory)
	s.env.OnActivity(inventoryManager.DiscoverInstanceInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverInstanceInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventoryInstanceTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryInstanceTestSuite))
}
