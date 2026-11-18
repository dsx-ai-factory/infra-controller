// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package namespace

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	tnsv1 "go.temporal.io/api/namespace/v1"
	tWorkflowv1 "go.temporal.io/api/workflowservice/v1"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/durationpb"
)

func TestEnsureRetention(t *testing.T) {
	describeErr := errors.New("describe failed")
	updateErr := errors.New("update failed")

	tests := []struct {
		name        string
		current     time.Duration
		retention   time.Duration
		describeErr error
		updateErr   error
		wantUpdate  bool
		wantErr     error
	}{
		{
			name:      "matching retention is left alone",
			current:   72 * time.Hour,
			retention: 72 * time.Hour,
		},
		{
			name:       "different retention is updated",
			current:    72 * time.Hour,
			retention:  168 * time.Hour,
			wantUpdate: true,
		},
		{
			name:        "describe error skips the update",
			retention:   168 * time.Hour,
			describeErr: describeErr,
			wantErr:     describeErr,
		},
		{
			name:       "update error is returned",
			current:    72 * time.Hour,
			retention:  168 * time.Hour,
			updateErr:  updateErr,
			wantUpdate: true,
			wantErr:    updateErr,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tnc := tmocks.NewNamespaceClient(t)

			var resp *tWorkflowv1.DescribeNamespaceResponse
			if tt.describeErr == nil {
				resp = &tWorkflowv1.DescribeNamespaceResponse{
					Config: &tnsv1.NamespaceConfig{WorkflowExecutionRetentionTtl: durationpb.New(tt.current)},
				}
			}
			tnc.On("Describe", mock.Anything, SiteNamespace).Return(resp, tt.describeErr)

			// Only the retention is set, so the update leaves the rest of the namespace config alone
			wantRequest := &tWorkflowv1.UpdateNamespaceRequest{
				Namespace: SiteNamespace,
				Config:    &tnsv1.NamespaceConfig{WorkflowExecutionRetentionTtl: durationpb.New(tt.retention)},
			}
			if tt.wantUpdate {
				tnc.On("Update", mock.Anything, mock.MatchedBy(func(req *tWorkflowv1.UpdateNamespaceRequest) bool {
					return proto.Equal(req, wantRequest)
				})).Return(tt.updateErr)
			}

			err := EnsureRetention(context.Background(), tnc, SiteNamespace, tt.retention)
			if tt.wantErr != nil {
				assert.ErrorIs(t, err, tt.wantErr)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
