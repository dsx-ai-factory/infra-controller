// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	activity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/testsuite"
)

func TestCreateExpectedRackGroupWritesCore(t *testing.T) {
	env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
	var manager activity.ManageExpectedRackGroup
	request := &corev1.ExpectedRackGroup{
		RackGroupId: &corev1.RackGroupId{Id: "nvl5-gp1-jhb01"},
		Topology:    "gb200-nvl72",
		RackIds:     []*corev1.RackId{{Id: "rack-01"}, {Id: "rack-02"}},
	}

	env.RegisterActivity(manager.CreateExpectedRackGroupOnSite)
	env.OnActivity(manager.CreateExpectedRackGroupOnSite, mock.Anything, request).Return(nil).Once()
	env.ExecuteWorkflow(CreateExpectedRackGroup, request)

	require.True(t, env.IsWorkflowCompleted())
	require.NoError(t, env.GetWorkflowError())
	env.AssertExpectations(t)
}

func TestReplaceAllExpectedRackGroupsWritesOneSnapshot(t *testing.T) {
	env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
	var manager activity.ManageExpectedRackGroup
	request := &corev1.ExpectedRackGroupList{ExpectedRackGroups: []*corev1.ExpectedRackGroup{{
		RackGroupId: &corev1.RackGroupId{Id: "nvl5-gp1-jhb01"},
		Topology:    "gb200-nvl72",
		RackIds:     []*corev1.RackId{{Id: "rack-01"}},
	}}}

	env.RegisterActivity(manager.ReplaceAllExpectedRackGroupsOnSite)
	env.OnActivity(manager.ReplaceAllExpectedRackGroupsOnSite, mock.Anything, request).Return(nil).Once()
	env.ExecuteWorkflow(ReplaceAllExpectedRackGroups, request)

	require.True(t, env.IsWorkflowCompleted())
	require.NoError(t, env.GetWorkflowError())
	env.AssertExpectations(t)
}
