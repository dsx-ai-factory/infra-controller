// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"context"
	"regexp"
	"testing"
	"time"

	"github.com/golang/mock/gomock"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	tBatchv1 "go.temporal.io/api/batch/v1"
	temporalEnums "go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	tWorkflowv1 "go.temporal.io/api/workflowservice/v1"
	twsv1mock "go.temporal.io/api/workflowservicemock/v1"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/testsuite"
	"google.golang.org/grpc"
	"google.golang.org/protobuf/proto"
)

func TestManageInventoryRetention_StartSuccessfulInventoryWorkflowDeletion(t *testing.T) {
	queryPattern := regexp.MustCompile(`^ExecutionStatus = 'Completed' AND CloseTime < '([^']+)' AND WorkflowType IN \('UpdateMachineInventory', .+, 'UpdateNVLinkLogicalPartitionInventory'\)$`)

	tests := []struct {
		name      string
		retention time.Duration
		startErr  error
		wantStart bool
		wantErr   bool
	}{
		{
			name:      "zero retention does not start a job",
			retention: 0,
		},
		{
			name:      "job deletes inventory workflows that completed before the cutoff",
			retention: 24 * time.Hour,
			wantStart: true,
		},
		{
			name:      "job started by an earlier attempt is not an error",
			retention: 24 * time.Hour,
			startErr:  serviceerror.NewWorkflowExecutionAlreadyStarted("batch job already started", "", ""),
			wantStart: true,
		},
		{
			name:      "run is skipped while another batch job is running",
			retention: 24 * time.Hour,
			startErr:  serviceerror.NewResourceExhausted(temporalEnums.RESOURCE_EXHAUSTED_CAUSE_CONCURRENT_LIMIT, "Max concurrent batch operations is reached"),
			wantStart: true,
		},
		{
			name:      "rate limit error is returned for a retry",
			retention: 24 * time.Hour,
			startErr:  serviceerror.NewResourceExhausted(temporalEnums.RESOURCE_EXHAUSTED_CAUSE_RPS_LIMIT, "namespace rate limit exceeded"),
			wantStart: true,
			wantErr:   true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tc := tmocks.NewClient(t)

			var gotRequest *tWorkflowv1.StartBatchOperationRequest
			if tt.wantStart {
				tws := twsv1mock.NewMockWorkflowServiceClient(gomock.NewController(t))
				tws.EXPECT().StartBatchOperation(gomock.Any(), gomock.Any()).DoAndReturn(
					func(_ context.Context, req *tWorkflowv1.StartBatchOperationRequest, _ ...grpc.CallOption) (*tWorkflowv1.StartBatchOperationResponse, error) {
						gotRequest = req
						return &tWorkflowv1.StartBatchOperationResponse{}, tt.startErr
					})
				tc.On("WorkflowService").Return(tws)
			}

			mir := NewManageInventoryRetention(tc, tt.retention)

			env := (&testsuite.WorkflowTestSuite{}).NewTestActivityEnvironment()
			env.RegisterActivity(&mir)

			earliest := time.Now().UTC().Add(-tt.retention).Truncate(time.Second)
			_, err := env.ExecuteActivity(mir.StartSuccessfulInventoryWorkflowDeletion)
			latest := time.Now().UTC().Add(-tt.retention)

			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}

			if !tt.wantStart {
				return
			}

			require.NotNil(t, gotRequest)
			assert.Contains(t, gotRequest.GetVisibilityQuery(), "'UpdateSitePrefixInventory'")

			match := queryPattern.FindStringSubmatch(gotRequest.GetVisibilityQuery())
			require.NotNil(t, match, "unexpected visibility query: %s", gotRequest.GetVisibilityQuery())

			cutoff, perr := time.Parse(time.RFC3339, match[1])
			require.NoError(t, perr)
			assert.False(t, cutoff.Before(earliest), "cutoff %s is before %s", cutoff, earliest)
			assert.False(t, cutoff.After(latest), "cutoff %s is after %s", cutoff, latest)

			wantRequest := &tWorkflowv1.StartBatchOperationRequest{
				Namespace:       "site",
				JobId:           "default-test-workflow-id-default-test-run-id",
				Reason:          "Delete inventory workflows that completed more than 24h0m0s ago",
				VisibilityQuery: gotRequest.GetVisibilityQuery(),
				Operation: &tWorkflowv1.StartBatchOperationRequest_DeletionOperation{
					DeletionOperation: &tBatchv1.BatchOperationDeletion{
						Identity: "default-test-workflow-id",
					},
				},
			}
			assert.True(t, proto.Equal(wantRequest, gotRequest), "unexpected batch request: %v", gotRequest)
		})
	}
}
