// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package redfish

import (
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
)

type getTaskURITestBody struct {
	io.Reader
	readErr    error
	closeCount int
}

func (b *getTaskURITestBody) Read(p []byte) (int, error) {
	if b.readErr != nil {
		return 0, b.readErr
	}
	return b.Reader.Read(p)
}

func (b *getTaskURITestBody) Close() error {
	b.closeCount++
	return nil
}

func TestRedfishClient_GetTaskURI(t *testing.T) {
	readErr := errors.New("injected task URI body read failure")
	cases := []struct {
		name            string
		body            string
		location        string
		readErr         error
		nilResponse     bool
		wantURI         string
		wantErr         string
		wantSyntaxError bool
	}{
		{
			name:     "JSON task ID takes precedence over Location",
			body:     `{"@odata.id":"/redfish/v1/TaskService/Tasks/42"}`,
			location: "/redfish/v1/TaskService/TaskMonitors/99",
			wantURI:  "/redfish/v1/TaskService/Tasks/42",
		},
		{
			name:     "missing JSON task ID falls back to Location",
			body:     `{}`,
			location: "/redfish/v1/TaskService/TaskMonitors/99",
			wantURI:  "/redfish/v1/TaskService/TaskMonitors/99",
		},
		{
			name:     "non-string JSON task ID falls back to Location",
			body:     `{"@odata.id":42}`,
			location: "/redfish/v1/TaskService/TaskMonitors/99",
			wantURI:  "/redfish/v1/TaskService/TaskMonitors/99",
		},
		{
			name:    "missing task metadata errors",
			body:    `{}`,
			wantErr: "could not find task URI in response",
		},
		{
			name:        "nil response errors",
			nilResponse: true,
			wantErr:     "nil response",
		},
		{
			name:     "empty body falls back to Location",
			location: "/redfish/v1/TaskService/TaskMonitors/99",
			wantURI:  "/redfish/v1/TaskService/TaskMonitors/99",
		},
		{
			name:            "empty body without Location errors",
			wantSyntaxError: true,
		},
		{
			name:            "malformed JSON errors despite Location",
			body:            `{"@odata.id":`,
			location:        "/redfish/v1/TaskService/TaskMonitors/99",
			wantSyntaxError: true,
		},
		{
			name:     "body read error propagates despite task metadata",
			body:     `{"@odata.id":"/redfish/v1/TaskService/Tasks/42"}`,
			location: "/redfish/v1/TaskService/TaskMonitors/99",
			readErr:  readErr,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var resp *http.Response
			var body *getTaskURITestBody
			if !tc.nilResponse {
				body = &getTaskURITestBody{Reader: strings.NewReader(tc.body), readErr: tc.readErr}
				resp = &http.Response{
					StatusCode: http.StatusAccepted,
					Header:     make(http.Header),
					Body:       body,
				}
				resp.Header.Set("Location", tc.location)
			}

			client := &RedfishClient{}
			uri, err := client.GetTaskURI(resp)
			assert.Equal(t, tc.wantURI, uri)
			if body != nil {
				assert.Equal(t, 1, body.closeCount, "response body must be closed on success and failure")
			}
			switch {
			case tc.readErr != nil:
				assert.ErrorIs(t, err, tc.readErr)
			case tc.wantSyntaxError:
				var syntaxErr *json.SyntaxError
				assert.ErrorAs(t, err, &syntaxErr)
			case tc.wantErr != "":
				assert.EqualError(t, err, tc.wantErr)
			default:
				assert.NoError(t, err)
			}
		})
	}
}
