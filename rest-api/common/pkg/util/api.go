// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"net/http"

	"github.com/labstack/echo/v4"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
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

// APIError represents a structured API error
type APIError struct {
	Code    int    `json:"-"`
	Source  string `json:"source"`
	Message string `json:"message"`
	Data    error  `json:"data"`
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

// NewAPIErrorResponse SENDS an API error response given appropriate params
// An error is returned to the caller if the send fails.
func NewAPIErrorResponse(c echo.Context, code int, message string, data error) error {
	apiNameIfc := c.Get(APINameContextKey)
	apiName, _ := apiNameIfc.(string)

	// This is the single choke point for error responses, whether sent by
	// handlers directly or via DefaultHTTPErrorHandler.
	cotel.RecordHTTPError(c.Request().Context(), code)

	return c.JSON(code, APIError{
		Code:    code,
		Source:  apiName,
		Message: message,
		Data:    data,
	})
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

	// Send response (span error recording happens inside NewAPIErrorResponse)
	if c.Request().Method == http.MethodHead { // Issue #608
		err = c.NoContent(code)
	} else {
		err = NewAPIErrorResponse(c, code, message, data)
	}
	if err != nil {
		e.Logger.Error(err)
	}
}
