// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package grpcproxy

import (
	"encoding/json"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestTimeoutLadder(t *testing.T) {
	cases := []struct {
		name    string
		shorter time.Duration
		longer  time.Duration
		why     string
	}{
		{
			name:    "activity finishes before the workflow expires",
			shorter: ActivityStartToCloseTimeout,
			longer:  WorkflowExecutionTimeout,
			why:     "a workflow that expires first reports a timeout the activity could still have answered",
		},
		{
			name:    "workflow expires before the caller gives up",
			shorter: WorkflowExecutionTimeout,
			longer:  cutil.WorkflowContextTimeout,
			why:     "the caller must outlast the workflow so a terminal result is observed rather than reported as a client-side timeout",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Less(t, tc.shorter, tc.longer, tc.why)
		})
	}
}

// TestBackendNames pins the identifiers Temporal and the proto registry
// dispatch on. Renaming a workflow type strands executions older workers
// created, and a service name that does not match the linked bindings fails
// every method resolution at runtime rather than at compile time.
func TestBackendNames(t *testing.T) {
	cases := []struct {
		name         string
		backend      Backend
		service      string
		workflowName string
	}{
		{name: "core", backend: Core, service: "forge.Forge", workflowName: "InvokeCoreGRPC"},
		{name: "flow", backend: Flow, service: "v1.Flow", workflowName: "InvokeFlowGRPC"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.service, tc.backend.ServiceName)
			assert.Equal(t, tc.workflowName, tc.backend.WorkflowName)
		})
	}
}

// TestPayloadShape pins the JSON both backends already write into Temporal
// history. The two contracts were separate types before they were merged, so a
// changed key or a dropped omitempty would make a worker unable to read a
// payload an older cloud release enqueued.
func TestPayloadShape(t *testing.T) {
	cases := []struct {
		name     string
		payload  any
		expected string
	}{
		{
			name:     "populated request",
			payload:  Request{FullMethod: "/v1.Flow/Version", RequestJSON: json.RawMessage(`{"a":1}`), EncryptedSecrets: []byte{0x01, 0x02}},
			expected: `{"fullMethod":"/v1.Flow/Version","requestJson":{"a":1},"encryptedSecrets":"AQI="}`,
		},
		{
			name:     "empty request omits optional fields",
			payload:  Request{FullMethod: "Version"},
			expected: `{"fullMethod":"Version"}`,
		},
		{
			name:     "populated response",
			payload:  Response{ResponseJSON: json.RawMessage(`{"b":2}`)},
			expected: `{"responseJson":{"b":2}}`,
		},
		{
			name:     "empty response",
			payload:  Response{},
			expected: `{}`,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			encoded, err := json.Marshal(tc.payload)
			require.NoError(t, err)
			assert.JSONEq(t, tc.expected, string(encoded))
		})
	}
}
