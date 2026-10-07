// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	mActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
)

type MachineWorkflowTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *MachineWorkflowTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *MachineWorkflowTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *MachineWorkflowTestSuite) Test_UpdateMachineInventory_Success() {
	var machineManager mActivity.ManageMachine

	request := &corev1.MaintenanceRequest{
		Operation: corev1.MaintenanceOperation_Enable,
		HostId:    &corev1.MachineId{Id: uuid.New().String()},
		Reference: util.GetStrPtr("Machine needs to taken offline to re-cable the network"),
	}

	// Mock UpdateVpcViaSiteAgent activity
	s.env.RegisterActivity(machineManager.SetMachineMaintenanceOnSite)
	s.env.OnActivity(machineManager.SetMachineMaintenanceOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateMachineInventory workflow
	s.env.ExecuteWorkflow(SetMachineMaintenance, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *MachineWorkflowTestSuite) Test_UpdateMachineInventory_ActivityFails() {
	var machineManager mActivity.ManageMachine

	request := &corev1.MaintenanceRequest{
		Operation: corev1.MaintenanceOperation_Enable,
		HostId:    &corev1.MachineId{Id: uuid.New().String()},
		Reference: util.GetStrPtr("Machine needs to taken offline to re-cable the network"),
	}

	errMsg := "Site Controller communication error"

	// Mock SetMachineMaintenanceOnSite activity failure
	s.env.RegisterActivity(machineManager.SetMachineMaintenanceOnSite)
	s.env.OnActivity(machineManager.SetMachineMaintenanceOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute SetMachineMaintenanceOnSite workflow
	s.env.ExecuteWorkflow(SetMachineMaintenance, request)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func (s *MachineWorkflowTestSuite) Test_CollectAndPublishMachineInventory_Success() {
	var machineInventoryManager mActivity.ManageMachineInventory

	// Mock SetMachineMaintenanceOnSite activity failure
	s.env.RegisterActivity(machineInventoryManager.CollectAndPublishMachineInventory)
	s.env.OnActivity(machineInventoryManager.CollectAndPublishMachineInventory, mock.Anything).Return(nil)

	// execute UpdateMachineInventory workflow
	s.env.ExecuteWorkflow(CollectAndPublishMachineInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *MachineWorkflowTestSuite) Test_CollectAndPublishMachineInventory_ActivityFails() {
	var machineInventoryManager mActivity.ManageMachineInventory

	errMsg := "Site Controller communication error"

	// Mock SetMachineMaintenanceOnSite activity failure
	s.env.RegisterActivity(machineInventoryManager.CollectAndPublishMachineInventory)
	s.env.OnActivity(machineInventoryManager.CollectAndPublishMachineInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute SetMachineMaintenanceOnSite workflow
	s.env.ExecuteWorkflow(CollectAndPublishMachineInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func (s *MachineWorkflowTestSuite) Test_UpdateMachineMetadata_Success() {
	var machineManager mActivity.ManageMachine

	request := &corev1.MachineMetadataUpdateRequest{
		MachineId: &corev1.MachineId{Id: uuid.New().String()},
		Metadata: &corev1.Metadata{
			Labels: []*corev1.Label{
				{
					Key:   "test-key",
					Value: util.GetStrPtr("test-value"),
				},
			},
		},
	}

	// Mock UpdateMachineMetadataOnSite activity
	s.env.RegisterActivity(machineManager.UpdateMachineMetadataOnSite)
	s.env.OnActivity(machineManager.UpdateMachineMetadataOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateMachineMetadata workflow
	s.env.ExecuteWorkflow(UpdateMachineMetadata, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *MachineWorkflowTestSuite) Test_UpdateMachineMetadata_ActivityFails() {
	var machineManager mActivity.ManageMachine

	errMsg := "Site Controller communication error"

	request := &corev1.MachineMetadataUpdateRequest{
		MachineId: &corev1.MachineId{Id: uuid.New().String()},
		Metadata: &corev1.Metadata{
			Labels: []*corev1.Label{
				{
					Key:   "test-key",
					Value: util.GetStrPtr("test-value"),
				},
			},
		},
	}

	// Mock UpdateMachineMetadataOnSite activity failure
	s.env.RegisterActivity(machineManager.UpdateMachineMetadataOnSite)
	s.env.OnActivity(machineManager.UpdateMachineMetadataOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute UpdateMachineMetadata workflow
	s.env.ExecuteWorkflow(UpdateMachineMetadata, request)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func (s *MachineWorkflowTestSuite) Test_CreateMachineHealthReport_Success() {
	var machineManager mActivity.ManageMachine
	req := &corev1.InsertMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: uuid.New().String()},
		HealthReportEntry: &corev1.HealthReportEntry{
			Report: &corev1.HealthReport{
				Source: "request-online-repair",
				Alerts: []*corev1.HealthProbeAlert{
					{Id: "OnLineRepair", Message: `{"details":"d","issue_category":"OTHER","summary":"s"}`},
				},
			},
			Mode: corev1.HealthReportApplyMode_Merge,
		},
	}
	s.env.RegisterActivity(machineManager.CreateMachineHealthReportOnSite)
	s.env.OnActivity(machineManager.CreateMachineHealthReportOnSite, mock.Anything, mock.Anything).Return(nil)
	s.env.ExecuteWorkflow(CreateMachineHealthReport, req)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *MachineWorkflowTestSuite) Test_DeleteMachineHealthReport_Success() {
	var machineManager mActivity.ManageMachine
	req := &corev1.RemoveMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: uuid.New().String()},
		Source:    "request-online-repair",
	}
	s.env.RegisterActivity(machineManager.DeleteMachineHealthReportOnSite)
	s.env.OnActivity(machineManager.DeleteMachineHealthReportOnSite, mock.Anything, mock.Anything).Return(nil)
	s.env.ExecuteWorkflow(DeleteMachineHealthReport, req)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func TestMachineWorkflowSuite(t *testing.T) {
	suite.Run(t, new(MachineWorkflowTestSuite))
}

// GetDpuMachinesTestSuite defines Temporal test suite for the GetDpuMachines workflow
type GetDpuMachinesTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *GetDpuMachinesTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *GetDpuMachinesTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *GetDpuMachinesTestSuite) Test_GetDpuMachines_Success() {
	var machineManager mActivity.ManageMachine

	dpuMachineIDs := []string{"dpu-machine-1", "dpu-machine-2", "dpu-machine-3"}

	expectedResult := &corev1.DpuMachineList{Machines: []*corev1.DpuMachine{
		{
			Machine: &corev1.Machine{
				Id: &corev1.MachineId{Id: "dpu-machine-1"},
			},
			DpuNetworkConfig: &corev1.ManagedHostNetworkConfigResponse{
				VniDevice:    "vxlan48",
				IsPrimaryDpu: true,
				NetworkSecurityPolicyOverrides: []*corev1.ResolvedNetworkSecurityGroupRule{
					{
						Rule: &corev1.NetworkSecurityGroupRuleAttributes{
							SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "10.0.0.0/24"},
							DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "10.1.0.0/24"},
						},
					},
				},
			},
		},
		{
			Machine: &corev1.Machine{
				Id: &corev1.MachineId{Id: "dpu-machine-2"},
			},
			DpuNetworkConfig: &corev1.ManagedHostNetworkConfigResponse{
				VniDevice:    "vxlan48",
				IsPrimaryDpu: false,
			},
		},
		{
			Machine: &corev1.Machine{
				Id: &corev1.MachineId{Id: "dpu-machine-3"},
			},
			DpuNetworkConfig: &corev1.ManagedHostNetworkConfigResponse{
				VniDevice:    "vxlan48",
				IsPrimaryDpu: false,
			},
		},
	}}

	// Mock GetDpuMachinesByIDs activity success
	s.env.RegisterActivity(machineManager.GetDpuMachinesByIDs)
	s.env.OnActivity(machineManager.GetDpuMachinesByIDs, mock.Anything, mock.Anything).Return(expectedResult, nil)

	// Execute GetDpuMachines workflow
	s.env.ExecuteWorkflow(GetDpuMachines, dpuMachineIDs)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())

	var result corev1.DpuMachineList
	s.env.GetWorkflowResult(&result)

	s.Equal(len(expectedResult.Machines), len(result.Machines))
	for i, dpuMachine := range result.Machines {
		s.Equal(expectedResult.Machines[i].Machine.Id.Id, dpuMachine.Machine.Id.Id)
		s.Equal(expectedResult.Machines[i].DpuNetworkConfig.VniDevice, dpuMachine.DpuNetworkConfig.VniDevice)
		s.Equal(expectedResult.Machines[i].DpuNetworkConfig.IsPrimaryDpu, dpuMachine.DpuNetworkConfig.IsPrimaryDpu)
	}
	s.Equal("10.0.0.0/24", result.Machines[0].DpuNetworkConfig.NetworkSecurityPolicyOverrides[0].Rule.GetSrcPrefix())
	s.Equal("10.1.0.0/24", result.Machines[0].DpuNetworkConfig.NetworkSecurityPolicyOverrides[0].Rule.GetDstPrefix())
}

func (s *GetDpuMachinesTestSuite) Test_GetDpuMachines_ActivityFails() {
	var machineManager mActivity.ManageMachine

	dpuMachineIDs := []string{"dpu-machine-1", "dpu-machine-2", "dpu-machine-3"}

	errMsg := "Site Controller communication error"

	// Mock GetDpuMachinesByIDs activity failure
	s.env.RegisterActivity(machineManager.GetDpuMachinesByIDs)
	s.env.OnActivity(machineManager.GetDpuMachinesByIDs, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// Execute GetDpuMachines workflow
	s.env.ExecuteWorkflow(GetDpuMachines, dpuMachineIDs)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestGetDpuMachinesTestSuite(t *testing.T) {
	suite.Run(t, new(GetDpuMachinesTestSuite))
}
