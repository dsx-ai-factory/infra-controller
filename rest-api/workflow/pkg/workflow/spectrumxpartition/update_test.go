// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package spectrumxpartition

import (
	"errors"
	"testing"

	sxpActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/spectrumxpartition"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type UpdateSpectrumXPartitionTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateSpectrumXPartitionTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateSpectrumXPartitionTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func testSpectrumXPartitionInventory() *corev1.SpectrumXPartitionInventory {
	return &corev1.SpectrumXPartitionInventory{
		SpxPartitions: []*corev1.SpxPartition{
			{Id: &corev1.SpxPartitionId{Value: uuid.NewString()}},
			{Id: &corev1.SpxPartitionId{Value: uuid.NewString()}},
		},
	}
}

func (s *UpdateSpectrumXPartitionTestSuite) Test_UpdateSpectrumXPartitionInventory_Success() {
	var sxpManager sxpActivity.ManageSpectrumXPartition

	s.env.RegisterActivity(sxpManager.UpdateSpectrumXPartitionsInDB)
	s.env.OnActivity(sxpManager.UpdateSpectrumXPartitionsInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	s.env.ExecuteWorkflow(UpdateSpectrumXPartitionInventory, uuid.NewString(), testSpectrumXPartitionInventory())
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateSpectrumXPartitionTestSuite) Test_UpdateSpectrumXPartitionInventory_ActivityFails() {
	var sxpManager sxpActivity.ManageSpectrumXPartition

	s.env.RegisterActivity(sxpManager.UpdateSpectrumXPartitionsInDB)
	s.env.OnActivity(sxpManager.UpdateSpectrumXPartitionsInDB, mock.Anything, mock.Anything, mock.Anything).Return(errors.New("UpdateSpectrumXPartitionInventory Failure"))

	s.env.ExecuteWorkflow(UpdateSpectrumXPartitionInventory, uuid.NewString(), testSpectrumXPartitionInventory())
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateSpectrumXPartitionInventory Failure", applicationErr.Error())
}

// An unparseable Site ID has to fail before the activity runs, since the activity takes a
// parsed UUID and would otherwise receive the zero value.
func (s *UpdateSpectrumXPartitionTestSuite) Test_UpdateSpectrumXPartitionInventory_InvalidSiteID() {
	var sxpManager sxpActivity.ManageSpectrumXPartition

	s.env.RegisterActivity(sxpManager.UpdateSpectrumXPartitionsInDB)

	s.env.ExecuteWorkflow(UpdateSpectrumXPartitionInventory, "not-a-uuid", testSpectrumXPartitionInventory())
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
	s.env.AssertNotCalled(s.T(), "UpdateSpectrumXPartitionsInDB")
}

func TestUpdateSpectrumXPartitionSuite(t *testing.T) {
	suite.Run(t, new(UpdateSpectrumXPartitionTestSuite))
}
