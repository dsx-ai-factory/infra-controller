// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operatingsystem

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	osImageActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/operatingsystem"
)

type UpdateOsImageTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateOsImageTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateOsImageTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateOsImageTestSuite) Test_UpdateOsImageInventory_Success() {
	var osImageManager osImageActivity.ManageOsImage

	siteID := uuid.New()
	osIDs := []uuid.UUID{uuid.New(), uuid.New()}

	osImageInventory := &corev1.OsImageInventory{
		OsImages: []*corev1.OsImage{
			{
				Attributes: &corev1.OsImageAttributes{
					Id: &corev1.UUID{Value: osIDs[0].String()},
				},
				Status: corev1.OsImageStatus_ImageReady,
			},
			{
				Attributes: &corev1.OsImageAttributes{
					Id: &corev1.UUID{Value: osIDs[1].String()},
				},
				Status: corev1.OsImageStatus_ImageFailed,
			},
		},
	}

	// Mock UpdateSSHKeyGroupsInDB activity
	s.env.RegisterActivity(osImageManager.UpdateOsImagesInDB)
	s.env.OnActivity(osImageManager.UpdateOsImagesInDB, mock.Anything, mock.Anything, mock.Anything).Return(osIDs, nil)
	s.env.OnActivity(osImageManager.UpdateOperatingSystemStatusInDB, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateOsImageInventory workflow
	s.env.ExecuteWorkflow(UpdateOsImageInventory, siteID.String(), osImageInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateOsImageTestSuite) Test_UpdateOsImageInventory_ActivityFails() {
	var osImageManager osImageActivity.ManageOsImage

	siteID := uuid.New()
	osIDs := []uuid.UUID{uuid.New(), uuid.New()}

	osImageInventory := &corev1.OsImageInventory{
		OsImages: []*corev1.OsImage{
			{
				Attributes: &corev1.OsImageAttributes{
					Id: &corev1.UUID{Value: osIDs[0].String()},
				},
				Status: corev1.OsImageStatus_ImageReady,
			},
			{
				Attributes: &corev1.OsImageAttributes{
					Id: &corev1.UUID{Value: osIDs[1].String()},
				},
				Status: corev1.OsImageStatus_ImageFailed,
			},
		},
	}

	// Mock UpdateVpcsViaSiteAgent activity failure
	s.env.RegisterActivity(osImageManager.UpdateOsImagesInDB)
	s.env.OnActivity(osImageManager.UpdateOsImagesInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil, errors.New("UpdateOsImageInventory Failure"))

	// execute UpdateVPCStatus workflow
	s.env.ExecuteWorkflow(UpdateOsImageInventory, siteID.String(), osImageInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateOsImageInventory Failure", applicationErr.Error())
}

func TestUpdateOsImageSuite(t *testing.T) {
	suite.Run(t, new(UpdateOsImageTestSuite))
}
