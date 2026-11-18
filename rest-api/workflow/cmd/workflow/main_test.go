// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"slices"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"

	tsdkClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tsdkWorker "go.temporal.io/sdk/worker"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
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
		name                         string
		namespace                    string
		successfulInventoryRetention time.Duration
		failCron                     string
		wantCrons                    []string
		wantErr                      error
	}{
		{
			// Both workers read the same config, so the Cloud worker also sees the cleanup enabled
			name:                         "Cloud worker triggers every cron before it runs",
			namespace:                    cwfn.CloudNamespace,
			successfulInventoryRetention: config.DefaultSiteNamespaceSuccessfulInventoryRetention,
			wantCrons:                    []string{"site-monitor-health-all", "rotate-certs-and-otps", "monitor-site-temporal-namespaces"},
			wantErr:                      errWorkerStopped,
		},
		{
			name:      "Cloud worker does not run when a cron fails to trigger",
			namespace: cwfn.CloudNamespace,
			failCron:  "rotate-certs-and-otps",
			wantErr:   errTriggerFailed,
		},
		{
			name:                         "Site worker triggers the successful inventory cleanup before it runs",
			namespace:                    cwfn.SiteNamespace,
			successfulInventoryRetention: config.DefaultSiteNamespaceSuccessfulInventoryRetention,
			wantCrons:                    []string{"delete-successful-inventory-workflows"},
			wantErr:                      errWorkerStopped,
		},
		{
			name:                         "Site worker does not run when the cleanup fails to trigger",
			namespace:                    cwfn.SiteNamespace,
			successfulInventoryRetention: config.DefaultSiteNamespaceSuccessfulInventoryRetention,
			failCron:                     "delete-successful-inventory-workflows",
			wantErr:                      errTriggerFailed,
		},
		{
			name:      "Site worker triggers no cron when successful inventory retention is disabled",
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

			err := runWorker(context.Background(), tc, w, tt.namespace, tt.successfulInventoryRetention, nil)
			require.ErrorIs(t, err, tt.wantErr)
			assert.ElementsMatch(t, tt.wantCrons, triggeredBeforeRun)
		})
	}
}

func TestRegisterHealthRoutes(t *testing.T) {
	tests := []struct {
		name       string
		method     string
		path       string
		wantStatus int
		wantAllow  string
		wantBody   string
	}{
		{
			name:       "GET /healthz reports healthy",
			method:     http.MethodGet,
			path:       "/healthz",
			wantStatus: http.StatusOK,
			wantBody:   `{"is_healthy":true,"error":null}`,
		},
		{
			name:       "GET /readyz reports healthy",
			method:     http.MethodGet,
			path:       "/readyz",
			wantStatus: http.StatusOK,
			wantBody:   `{"is_healthy":true,"error":null}`,
		},
		{
			name:       "POST /healthz is rejected",
			method:     http.MethodPost,
			path:       "/healthz",
			wantStatus: http.StatusMethodNotAllowed,
			wantAllow:  "GET, HEAD",
			wantBody:   "Method Not Allowed\n",
		},
		{
			name:       "PUT /readyz is rejected",
			method:     http.MethodPut,
			path:       "/readyz",
			wantStatus: http.StatusMethodNotAllowed,
			wantAllow:  "GET, HEAD",
			wantBody:   "Method Not Allowed\n",
		},
	}

	mux := http.NewServeMux()
	registerHealthRoutes(mux)

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			response := httptest.NewRecorder()
			mux.ServeHTTP(response, httptest.NewRequest(tt.method, tt.path, nil))

			assert.Equal(t, tt.wantStatus, response.Code)
			assert.Equal(t, tt.wantAllow, response.Header().Get("Allow"))
			assert.Equal(t, tt.wantBody, response.Body.String())
		})
	}
}
