// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package user

import (
	"context"
	"testing"

	cloudutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/testsuite"

	userActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/user"
)

type UnitTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UnitTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UnitTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UnitTestSuite) Test_UpdateUserFromNGC_Success() {
	userID := uuid.New()
	starfleetID := "test12345"
	ngcToken := "test67890"
	encryptedNgcToken := cloudutils.EncryptData([]byte(ngcToken), starfleetID)

	ngcUser := userActivity.NgcUser{
		Email: "jdoe@test.com",
		Name:  "John Doe",
		Roles: []userActivity.NgcOrgRole{
			{
				Org: userActivity.NgcOrg{
					ID:          123,
					Name:        "Test Org",
					OrgType:     "test-org-type",
					Description: "Test Org Description",
				},
				OrgRoles: []string{"test-org-role"},
			},
		},
	}

	var userProfile userActivity.ManageUser

	s.env.OnActivity(userProfile.GetUserDataFromNgc, mock.AnythingOfType("*context.timerCtx"), userID, encryptedNgcToken).Return(
		func(ctx context.Context, actualUserID uuid.UUID, actualEncryptedNgcToken []byte) (*userActivity.NgcUser, error) {
			s.Equal(userID, actualUserID)
			s.Equal(encryptedNgcToken, actualEncryptedNgcToken)
			return &ngcUser, nil
		})

	s.env.OnActivity(userProfile.UpdateUserInDB, mock.AnythingOfType("*context.timerCtx"), userID, &ngcUser).Return(
		func(ctx context.Context, actualUserID uuid.UUID, actualNgcUser *userActivity.NgcUser) error {
			s.Equal(userID, actualUserID)
			s.Equal(&ngcUser, actualNgcUser)
			return nil
		})

	s.env.ExecuteWorkflow(UpdateUserFromNGC, userID, encryptedNgcToken, false)

	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UnitTestSuite) Test_ExecuteUpdateUserFromNGCWorkflow_Success() {
	ctx := context.Background()
	userID := uuid.New()
	starfleetID := "test12345"
	ngcToken := "test67890"
	immediate := false

	wid := "test-workflow-id"

	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	tc := &tmocks.Client{}

	tc.Mock.On("ExecuteWorkflow", context.Background(), mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, []uint8, bool) error"), userID, mock.AnythingOfType("[]uint8"),
		immediate).Return(wrun, nil)

	rwid, err := ExecuteUpdateUserFromNGCWorkflow(ctx, tc, userID, starfleetID, ngcToken, immediate)
	s.NoError(err)
	s.Equal(wid, *rwid)
}

func TestUnitTestSuite(t *testing.T) {
	suite.Run(t, new(UnitTestSuite))
}
