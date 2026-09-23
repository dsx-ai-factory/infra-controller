// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"net/http"

	"github.com/labstack/echo/v4"
)

const (
	// APINameContextKey is the context key for the API name
	APINameContextKey = "apiName"

	// APIErrorInternalServer indicates an unexpected error is reported from the API
	APIErrorInternalServer = "An unexpected error occurred while processing the request"

	// APIErrorNotFound indicates that the requested path was not found
	APIErrorNotFound = "The requested path was not found"
)

var (
	// ErrBadRequest (400) is returned for bad request (validation)
	ErrBadRequest = echo.ErrBadRequest

	// ErrUnauthorized (401) is returned when user is not authorized
	ErrUnauthorized = echo.ErrUnauthorized

	// ErrInternal (500) is returned when an internal server error occurs
	ErrInternal = echo.ErrInternalServerError
)

// APIErrorRecoveryActionReconcile requires checking the outcome of an earlier
// mutation before issuing another one.
const APIErrorRecoveryActionReconcile = "Reconcile"

// APIError represents a structured API error
type APIError struct {
	Code    int    `json:"-"`
	Source  string `json:"source"`
	Message string `json:"message"`
	Data    error  `json:"data"`
	// Omit unclassified recovery metadata to preserve existing error bodies.
	Retryable         *bool  `json:"retryable,omitempty"`
	RetryAfterSeconds *int32 `json:"retryAfterSeconds,omitempty"`
	RecoveryAction    string `json:"recoveryAction,omitempty"`
}

// Error implements the error interface so *APIError can flow through error
// channels (e.g., closures returned by WithTx). Use NewAPIErrorResponse to
// turn one into an Echo response at the API boundary.
func (a *APIError) Error() string {
	return a.Message
}

// Unwrap returns the internal error recorded in Data, so errors.Is and
// errors.As reach the cause behind an APIError.
func (a *APIError) Unwrap() error {
	return a.Data
}

// Diagnosis returns the error worth logging: the internal cause, or a itself
// when the cause was folded into Message and Unwrap is therefore nil.
func (a *APIError) Diagnosis() error {
	if cause := a.Unwrap(); cause != nil {
		return cause
	}
	return a
}

// NewAPIError returns an API error given appropriate params
func NewAPIError(code int, message string, data error) *APIError {
	return &APIError{
		Code:    code,
		Message: message,
		Data:    data,
	}
}

// WithRetryable classifies a definite rejection. True permits a bounded retry;
// it does not promise availability or eventual success.
func (a *APIError) WithRetryable(retryable bool) *APIError {
	a.Retryable = &retryable
	if !retryable {
		a.RetryAfterSeconds = nil
	}
	a.RecoveryAction = ""
	return a
}

// WithReconciliation marks an uncertain mutation outcome, not permission to retry.
func (a *APIError) WithReconciliation() *APIError {
	a = a.WithRetryable(false)
	a.RecoveryAction = APIErrorRecoveryActionReconcile
	return a
}

// Send preserves the error metadata and sets the response's API source.
func (a *APIError) Send(c echo.Context) error {
	response := *a
	response.Source, _ = c.Get(APINameContextKey).(string)
	return c.JSON(response.Code, response)
}

// NewAPIErrorResponse sends an unclassified API error response.
func NewAPIErrorResponse(c echo.Context, code int, message string, data error) error {
	return NewAPIError(code, message, data).Send(c)
}

// DefaultHTTPErrorHandler is the default HTTP error handler. It sends a structured error response
//
// NOTE: In case errors happens in middleware call-chain that is returning from handler (handler ran into un-recovered error):
// When handler has already sent response (ala c.JSON()) and there is error in middleware that is returning from
// handler, then the error that global error handler received will be ignored because we have already "committed" the
// response and status code header has been sent to the client.
func DefaultHTTPErrorHandler(err error, c echo.Context) {
	if c.Response().Committed {
		return
	}

	he, ok := err.(*echo.HTTPError)
	if ok {
		if he.Internal != nil {
			if herr, sok := he.Internal.(*echo.HTTPError); sok {
				he = herr
			}
		}
	} else {
		he = &echo.HTTPError{
			Code:    http.StatusInternalServerError,
			Message: APIErrorInternalServer,
		}
	}

	e := c.Echo()

	// Issue #1426
	code := he.Code

	var message string
	var data error

	message, ok = he.Message.(string)

	if ok {
		if e.Debug {
			data = err
		}
	} else {
		message = APIErrorInternalServer
	}

	// Override 404
	if code == http.StatusNotFound {
		message = APIErrorNotFound
	} else if code == http.StatusInternalServerError {
		message = APIErrorInternalServer
	}

	// Send response
	if c.Request().Method == http.MethodHead { // Issue #608
		err = c.NoContent(code)
	} else {
		err = NewAPIErrorResponse(c, code, message, data)
	}
	if err != nil {
		e.Logger.Error(err)
	}
}
