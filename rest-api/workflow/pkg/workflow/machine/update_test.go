// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package machine

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	machineActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/machine"
)

type UpdateMachineInventoryTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateMachineInventoryTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateMachineInventoryTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateMachineInventoryTestSuite) Test_UpdateMachineInventory_Success() {
	var machineManager machineActivity.ManageMachine

	siteID := uuid.New()

	machineInfo := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:    &corev1.MachineId{Id: uuid.New().String()},
			State: "Running",
		},
	}

	machineInventory := &corev1.MachineInventory{
		Machines:  []*corev1.MachineInfo{machineInfo},
		Timestamp: timestamppb.Now(),
	}

	// Mock UpdateVpcViaSiteAgent activity
	s.env.RegisterActivity(machineManager.UpdateMachinesInDB)
	s.env.OnActivity(machineManager.UpdateMachinesInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateMachineInventory workflow
	s.env.ExecuteWorkflow(UpdateMachineInventory, siteID.String(), machineInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateMachineInventoryTestSuite) Test_UpdateMachineInventory_ActivityFails() {
	var machineManager machineActivity.ManageMachine

	siteID := uuid.New()

	machineInfo := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:    &corev1.MachineId{Id: uuid.New().String()},
			State: "Running",
		},
	}

	machineInventory := &corev1.MachineInventory{
		Machines:  []*corev1.MachineInfo{machineInfo},
		Timestamp: timestamppb.Now(),
	}

	// Mock UpdateMachinesInDB activity failure
	s.env.RegisterActivity(machineManager.UpdateMachinesInDB)
	s.env.OnActivity(machineManager.UpdateMachinesInDB, mock.Anything, mock.Anything, mock.Anything).Return(errors.New("UpdateMachineInventory Failure"))

	// execute UpdateMachineInventory workflow
	s.env.ExecuteWorkflow(UpdateMachineInventory, siteID.String(), machineInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateMachineInventory Failure", applicationErr.Error())
}

func TestUpdateMachineInventorySuite(t *testing.T) {
	suite.Run(t, new(UpdateMachineInventoryTestSuite))
}
