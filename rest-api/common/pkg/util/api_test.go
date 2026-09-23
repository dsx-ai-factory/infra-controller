// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
)

func TestNewAPIErrorResponse(t *testing.T) {
	type args struct {
		c       echo.Context
		status  int
		message string
		data    error
	}

	e := echo.New()
	req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(`{"test": true}`))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()

	ec := e.NewContext(req, rec)
	ec.Set(APINameContextKey, "test")

	tests := []struct {
		name string
		args args
	}{
		{
			name: "initialize and return error response",
			args: args{
				c:       ec,
				status:  400,
				message: "bad request",
				data:    errors.New("bad request"),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := NewAPIErrorResponse(tt.args.c, tt.args.status, tt.args.message, tt.args.data)
			assert.NoError(t, err)

			assert.Contains(t, rec.Body.String(), `"source":"test"`)
		})
	}
}

// TestAPIErrorUnwrapAndDiagnosis pins the split between the two accessors:
// Unwrap reports a missing cause as nil, because the errors package walks it
// without detecting a cycle, and Diagnosis is where the fallback to the
// APIError itself belongs.
func TestAPIErrorUnwrapAndDiagnosis(t *testing.T) {
	cause := errors.New("flow rejected the request")

	tests := []struct {
		name            string
		apiError        *APIError
		expectedUnwrap  error
		expectedLogged  string
		expectedMatches bool
	}{
		{
			name:            "cause recorded",
			apiError:        NewAPIError(http.StatusInternalServerError, "Failed to get Rack details", cause),
			expectedUnwrap:  cause,
			expectedLogged:  "flow rejected the request",
			expectedMatches: true,
		},
		{
			name:           "cause folded into the message",
			apiError:       NewAPIError(http.StatusNotFound, "Rack not found", nil),
			expectedUnwrap: nil,
			expectedLogged: "Rack not found",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.expectedUnwrap, tt.apiError.Unwrap())
			assert.Equal(t, tt.expectedLogged, tt.apiError.Diagnosis().Error())
			assert.Equal(t, tt.expectedMatches, errors.Is(tt.apiError, cause))
		})
	}
}

func TestDefaultHTTPErrorHandler(t *testing.T) {
	type args struct {
		err error
	}

	e := echo.New()

	tests := []struct {
		name            string
		args            args
		expectedStatus  int
		expectedMessage string
	}{
		{
			name: "test 404 error handler",
			args: args{
				err: echo.ErrNotFound,
			},
			expectedStatus:  http.StatusNotFound,
			expectedMessage: APIErrorNotFound,
		},
		{
			name: "test 500 error handler",
			args: args{
				err: echo.ErrInternalServerError,
			},
			expectedStatus:  http.StatusInternalServerError,
			expectedMessage: APIErrorInternalServer,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.Set("apiName", "test")

			DefaultHTTPErrorHandler(tt.args.err, ec)

			resp := ec.Response()
			assert.Equal(t, tt.expectedStatus, resp.Status)

			rst := &APIError{}
			err := json.Unmarshal(rec.Body.Bytes(), rst)
			assert.NoError(t, err)

			assert.Equal(t, "test", rst.Source)
			assert.Equal(t, tt.expectedMessage, rst.Message)
		})
	}
}

func TestAPIError_Send(t *testing.T) {
	for _, tt := range []struct {
		name   string
		apiErr *APIError
		body   string
	}{
		{"unclassified body unchanged", NewAPIError(400, "rejected", nil), `{"source":"nico","message":"rejected","data":null}`},
		{"retry permitted", NewAPIError(400, "rejected", nil).WithRetryable(true), `{"source":"nico","message":"rejected","data":null,"retryable":true}`},
		{"retry delay", &APIError{Code: 400, Message: "rejected", Retryable: GetPtr(true), RetryAfterSeconds: GetPtr(int32(5))}, `{"source":"nico","message":"rejected","data":null,"retryable":true,"retryAfterSeconds":5}`},
		{"conflict", NewAPIError(400, "rejected", nil).WithRetryable(false), `{"source":"nico","message":"rejected","data":null,"retryable":false}`},
		{"uncertain outcome", NewAPIError(500, "unknown", nil).WithReconciliation(), `{"source":"nico","message":"unknown","data":null,"retryable":false,"recoveryAction":"Reconcile"}`},
	} {
		t.Run(tt.name, func(t *testing.T) {
			rec := httptest.NewRecorder()
			c := echo.New().NewContext(httptest.NewRequest(http.MethodPost, "/", nil), rec)
			c.Set(APINameContextKey, "nico")
			assert.NoError(t, tt.apiErr.Send(c))
			assert.Equal(t, tt.apiErr.Code, rec.Code)
			assert.Equal(t, tt.body+"\n", rec.Body.String())
			assert.Empty(t, tt.apiErr.Source, "sending must not mutate the original error")
		})
	}
}
