// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package redfish

import (
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/stmcginnis/gofish"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

type taskStatusRoundTripper func(*http.Request) (*http.Response, error)

func (f taskStatusRoundTripper) RoundTrip(req *http.Request) (*http.Response, error) {
	return f(req)
}

type taskStatusResponseBody struct {
	io.Reader
	readErr error
	closed  bool
}

func (b *taskStatusResponseBody) Read(p []byte) (int, error) {
	if b.readErr != nil {
		return 0, b.readErr
	}
	return b.Reader.Read(p)
}

func (b *taskStatusResponseBody) Close() error {
	b.closed = true
	return nil
}

func TestRedfishClient_GetTaskStatus(t *testing.T) {
	transportErr := errors.New("task status transport failure")
	readErr := errors.New("task status body read failure")
	cases := []struct {
		name         string
		body         string
		transportErr error
		readErr      error
		invalidURI   bool
		wantState    string
		wantPercent  int
		wantErr      bool
	}{
		{
			name:        "decodes task progress",
			body:        `{"TaskState":"Running","PercentComplete":37}`,
			wantState:   "Running",
			wantPercent: 37,
		},
		{
			name: "missing optional fields retain zero values",
			body: `{}`,
		},
		{
			name: "incorrectly typed optional fields retain zero values",
			body: `{"TaskState":42,"PercentComplete":"37"}`,
		},
		{
			name:    "malformed JSON returns an error",
			body:    `{"TaskState":"Running",`,
			wantErr: true,
		},
		{
			name:         "transport failure is propagated",
			transportErr: transportErr,
			wantErr:      true,
		},
		{
			name:    "body read failure is propagated",
			readErr: readErr,
			wantErr: true,
		},
		{
			name:       "invalid task URI fails before transport",
			invalidURI: true,
			wantErr:    true,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			const endpoint = "https://bmc.example.test"
			const taskPath = "/redfish/v1/TaskService/Tasks/42"
			const username = "task-poller"
			const password = "test:p+a%ss"
			body := &taskStatusResponseBody{
				Reader:  strings.NewReader(tc.body),
				readErr: tc.readErr,
			}
			requests := 0
			transport := taskStatusRoundTripper(func(req *http.Request) (*http.Response, error) {
				requests++
				assert.Equal(t, http.MethodGet, req.Method)
				assert.Equal(t, taskPath, req.URL.Path)
				assert.Equal(t, endpoint+taskPath, req.URL.String())
				user, pass, ok := req.BasicAuth()
				assert.True(t, ok, "polling request must include Basic Auth")
				assert.Equal(t, username, user)
				assert.Equal(t, password, pass)
				if tc.transportErr != nil {
					return nil, tc.transportErr
				}
				return &http.Response{StatusCode: http.StatusOK, Body: body}, nil
			})
			client := &RedfishClient{
				APIClient: &gofish.APIClient{HTTPClient: &http.Client{Transport: transport}},
				ClientConfig: gofish.ClientConfig{
					Endpoint: endpoint,
					Username: username,
					Password: password,
				},
			}
			taskURI := taskPath
			wantRequests := 1
			if tc.invalidURI {
				taskURI += "\n"
				wantRequests = 0
			}

			state, percent, err := client.GetTaskStatus(taskURI)

			assert.Equal(t, wantRequests, requests)
			assert.Equal(t, !tc.invalidURI && tc.transportErr == nil, body.closed, "close every returned response body, including on decode/read errors")
			if tc.wantErr {
				require.Error(t, err)
				if tc.transportErr != nil {
					assert.ErrorIs(t, err, tc.transportErr)
				}
				if tc.readErr != nil {
					assert.ErrorIs(t, err, tc.readErr)
				}
			} else {
				require.NoError(t, err)
			}
			assert.Equal(t, tc.wantState, state)
			assert.Equal(t, tc.wantPercent, percent)
		})
	}
}
