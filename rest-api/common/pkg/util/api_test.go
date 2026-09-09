// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel/codes"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
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

func TestNewAPIErrorResponseDoesNotExportErrorDetails(t *testing.T) {
	recorder := tracetest.NewSpanRecorder()
	provider := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(recorder))
	ctx, span := provider.Tracer("test").Start(context.Background(), "request")

	e := echo.New()
	req := httptest.NewRequest(http.MethodGet, "/", nil).WithContext(ctx)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)

	const secret = "authorization=top-secret"
	require.NoError(t, NewAPIErrorResponse(
		ec,
		http.StatusInternalServerError,
		APIErrorInternalServer,
		errors.New(secret),
	))
	span.End()

	ended := recorder.Ended()
	require.Len(t, ended, 1)
	assert.Equal(t, codes.Error, ended[0].Status().Code)
	assert.Empty(t, ended[0].Status().Description)
	assert.Empty(t, ended[0].Events())

	var attributes []string
	for _, attr := range ended[0].Attributes() {
		attributes = append(attributes, fmt.Sprintf("%s=%v", attr.Key, attr.Value.AsInterface()))
	}
	joined := strings.Join(attributes, ",")
	assert.Contains(t, joined, "error.type=api.internal")
	assert.Contains(t, joined, "http.response.status_code=500")
	assert.NotContains(t, joined, secret)
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
