// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package utils

import (
	"bytes"
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestRetryWithExponentialBackoff(t *testing.T) {
	requestReceived := make(chan struct{}, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestReceived <- struct{}{}
		w.WriteHeader(http.StatusInternalServerError)
	}))
	t.Cleanup(server.Close)

	requestBody := []byte(`{"request":"body"}`)
	ctx, cancel := context.WithCancel(context.Background())
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, server.URL, bytes.NewReader(requestBody))
	require.NoError(t, err)

	retryDone := make(chan error, 1)
	go func() {
		response, retryErr := RetryWithExponentialBackoff(server.Client(), request, requestBody)
		if response != nil {
			_ = response.Body.Close()
		}
		retryDone <- retryErr
	}()
	<-requestReceived
	cancel()

	select {
	case retryErr := <-retryDone:
		require.ErrorIs(t, retryErr, context.Canceled)
	case <-time.After(250 * time.Millisecond):
		t.Fatal("retry backoff did not stop after context cancellation")
	}
}

func TestSiteHealth(t *testing.T) {
	tests := []struct {
		name            string
		coreStatus      CompStatus
		temporalStatus  CompStatus
		flowGrpcEnabled bool
		flowStatus      CompStatus
		expected        CompStatus
	}{
		{
			name:            "all required dependencies healthy with Flow disabled",
			coreStatus:      CompHealthy,
			temporalStatus:  CompHealthy,
			flowGrpcEnabled: false,
			flowStatus:      CompUnhealthy,
			expected:        CompHealthy,
		},
		{
			name:            "all enabled dependencies healthy",
			coreStatus:      CompHealthy,
			temporalStatus:  CompHealthy,
			flowGrpcEnabled: true,
			flowStatus:      CompHealthy,
			expected:        CompHealthy,
		},
		{
			name:            "enabled Flow unhealthy",
			coreStatus:      CompHealthy,
			temporalStatus:  CompHealthy,
			flowGrpcEnabled: true,
			flowStatus:      CompUnhealthy,
			expected:        CompUnhealthy,
		},
		{
			name:            "enabled Flow not initialized",
			coreStatus:      CompHealthy,
			temporalStatus:  CompHealthy,
			flowGrpcEnabled: true,
			flowStatus:      CompNotKnown,
			expected:        CompUnhealthy,
		},
		{
			name:            "Core unhealthy",
			coreStatus:      CompUnhealthy,
			temporalStatus:  CompHealthy,
			flowGrpcEnabled: false,
			flowStatus:      CompHealthy,
			expected:        CompUnhealthy,
		},
		{
			name:            "Temporal unhealthy",
			coreStatus:      CompHealthy,
			temporalStatus:  CompUnhealthy,
			flowGrpcEnabled: false,
			flowStatus:      CompHealthy,
			expected:        CompUnhealthy,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			elektra := elektratypes.NewElektraTypes()
			elektra.Conf.FlowGrpc.Enabled = test.flowGrpcEnabled
			elektra.Managers.CoreGrpc.State.HealthStatus.Store(uint64(test.coreStatus))
			elektra.Managers.Workflow.State.HealthStatus.Store(uint64(test.temporalStatus))
			elektra.Managers.FlowGrpc.State.HealthStatus.Store(uint64(test.flowStatus))

			assert.Equal(t, test.expected, SiteHealth(elektra))
		})
	}
}

func TestStatusPort(t *testing.T) {
	tests := []struct {
		name    string
		esaPort string
		want    string
	}{
		{name: "defaults when ESA_PORT is empty", esaPort: "", want: "8080"},
		{name: "uses ESA_PORT when set", esaPort: "9080", want: "9080"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Setenv("ESA_PORT", tt.esaPort)
			assert.Equal(t, tt.want, StatusPort())
		})
	}
}
