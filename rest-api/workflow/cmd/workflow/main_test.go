// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"

	tsdkClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tsdkWorker "go.temporal.io/sdk/worker"

	cwfn "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/namespace"
)

var (
	errWorkerStopped = errors.New("worker stopped")
	errTriggerFailed = errors.New("trigger failed")
)

// stubWorker calls onRun from Run, then stops as a worker would on shutdown.
type stubWorker struct {
	tsdkWorker.Worker
	onRun func()
}

func (s stubWorker) Run(<-chan interface{}) error {
	s.onRun()
	return errWorkerStopped
}

func TestRunWorker(t *testing.T) {
	tests := []struct {
		name      string
		namespace string
		failCron  string
		wantCrons []string
		wantErr   error
	}{
		{
			name:      "Cloud worker triggers every cron before it runs",
			namespace: cwfn.CloudNamespace,
			wantCrons: []string{"site-monitor-health-all", "rotate-certs-and-otps", "monitor-site-temporal-namespaces"},
			wantErr:   errWorkerStopped,
		},
		{
			name:      "Cloud worker does not run when a cron fails to trigger",
			namespace: cwfn.CloudNamespace,
			failCron:  "rotate-certs-and-otps",
			wantErr:   errTriggerFailed,
		},
		{
			name:      "Site worker triggers no cron",
			namespace: cwfn.SiteNamespace,
			wantErr:   errWorkerStopped,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var triggered, triggeredBeforeRun []string

			wrun := &tmocks.WorkflowRun{}
			wrun.On("GetID").Return("")

			tc := &tmocks.Client{}
			tc.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything).Return(
				func(_ context.Context, options tsdkClient.StartWorkflowOptions, _ interface{}, _ ...interface{}) (tsdkClient.WorkflowRun, error) {
					triggered = append(triggered, options.ID)
					if options.ID == tt.failCron {
						return nil, errTriggerFailed
					}
					return wrun, nil
				})

			w := stubWorker{onRun: func() { triggeredBeforeRun = slices.Clone(triggered) }}

			err := runWorker(context.Background(), tc, w, tt.namespace, nil)
			require.ErrorIs(t, err, tt.wantErr)
			assert.ElementsMatch(t, tt.wantCrons, triggeredBeforeRun)
		})
	}
}
