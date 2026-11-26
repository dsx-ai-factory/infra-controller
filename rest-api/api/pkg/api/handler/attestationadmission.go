// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json/jsontext"
	jsonv2 "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"net/url"
	"reflect"
	"unicode/utf8"

	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// The attestation endpoints forward operator text, such as matcher patterns
// and version preconditions, to Core and to the request audit log. Their
// admission is stricter than Echo binding so the strings Core and the audit
// log receive are exactly the ones the operator sent: no repaired UTF-8,
// silently dropped duplicates, or ignored members.

// admitAttestationQuery parses the raw query once and binds it into each of
// dsts when every key and value is valid UTF-8, no key repeats, and every key
// is a query tag of one of dsts. Pass no dsts for a route that accepts no
// query.
func admitAttestationQuery(c echo.Context, dsts ...any) *cutil.APIError {
	values, err := url.ParseQuery(c.Request().URL.RawQuery)
	if err != nil {
		return cutil.NewAPIError(http.StatusBadRequest, "Query string is malformed", nil)
	}
	for key, vals := range values {
		if !utf8.ValidString(key) {
			return cutil.NewAPIError(http.StatusBadRequest, "Query parameter names must be valid UTF-8", nil)
		}
		if len(vals) != 1 {
			return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Query parameter %s must be specified at most once", key), nil)
		}
		if !utf8.ValidString(vals[0]) {
			return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Query parameter %s must be valid UTF-8", key), nil)
		}
	}
	err = common.ValidateKnownQueryParams(values, dsts...)
	if err != nil {
		return cutil.NewAPIError(http.StatusBadRequest, err.Error(), nil)
	}
	for _, dst := range dsts {
		err = (&echo.DefaultBinder{}).BindQueryParams(c, dst)
		if err != nil {
			return cutil.NewAPIError(http.StatusBadRequest, "Query parameters have invalid types", nil)
		}
	}
	return nil
}

// attestationMachineID returns the machine ID path segment decoded exactly
// once. Echo captures parameters from URL.RawPath when the request used a
// non-canonical escape, so the segment is unescaped here in that case only.
func attestationMachineID(c echo.Context) (string, *cutil.APIError) {
	id := c.Param("id")
	if c.Request().URL.RawPath == "" {
		return id, nil
	}
	decoded, err := url.PathUnescape(id)
	if err != nil {
		return "", cutil.NewAPIError(http.StatusBadRequest, "machineId path segment is malformed", nil)
	}
	return decoded, nil
}

// sendAttestationAdmissionError renders a body admission failure. An Echo
// HTTP error, such as the server's 413 for an oversized body, is returned
// unchanged so Echo renders it; an APIError is sent with its own status.
func sendAttestationAdmissionError(c echo.Context, err error) error {
	var apiErr *cutil.APIError
	if errors.As(err, &apiErr) {
		return apiErr.Send(c)
	}
	return err
}

// requireEmptyAttestationBody reads the whole size-limited body, including for
// chunked requests, and rejects any byte. See sendAttestationAdmissionError
// for the error contract.
func requireEmptyAttestationBody(c echo.Context) error {
	body, err := io.ReadAll(c.Request().Body)
	if err != nil {
		return bodyReadError(err)
	}
	if len(body) > 0 {
		return cutil.NewAPIError(http.StatusBadRequest, "Request body must be empty", nil)
	}
	return nil
}

// errAttestationNullValue marks JSON null where the Go field is not a pointer.
var errAttestationNullValue = errors.New("must not be null")

// rejectNonPointerNull runs before json/v2 decodes each value. json/v2 leaves
// a non-pointer field unchanged on null, which would let null stand in for an
// omitted member, so null is accepted only where the field is a pointer.
var rejectNonPointerNull = jsonv2.UnmarshalFromFunc(func(dec *jsontext.Decoder, v any) error {
	if dec.PeekKind() == 'n' && reflect.TypeOf(v).Elem().Kind() != reflect.Pointer {
		return errAttestationNullValue
	}
	return errors.ErrUnsupported
})

// decodeClosedJSONObject admits one JSON object into dst, which must point to
// a struct. Failures are checked in this order: media type (415), read (the
// server's 413 for an oversized body), then decoding (400). json/v2 rejects
// invalid UTF-8, unpaired surrogate escapes, trailing values, repeated
// members, and members other than dst's exact json names at every level, and
// it decodes strings without repairing them. See
// sendAttestationAdmissionError for the error contract.
func decodeClosedJSONObject(c echo.Context, dst any) error {
	mediaType, _, err := mime.ParseMediaType(c.Request().Header.Get(echo.HeaderContentType))
	if err != nil || mediaType != echo.MIMEApplicationJSON {
		return cutil.NewAPIError(http.StatusUnsupportedMediaType, "Request body must be application/json", nil)
	}
	body, err := io.ReadAll(c.Request().Body)
	if err != nil {
		return bodyReadError(err)
	}
	err = jsonv2.Unmarshal(body, dst, jsonv2.RejectUnknownMembers(true), jsonv2.WithUnmarshalers(rejectNonPointerNull))
	if err != nil {
		return cutil.NewAPIError(http.StatusBadRequest, attestationJSONErrorMessage(err), nil)
	}
	return nil
}

// attestationJSONErrorMessage describes a decoding failure without echoing
// request content. json/v2 error text quotes input bytes, so only the error
// class and a JSON pointer are used. For repeated and unknown members the
// pointer ends with the offending name, so only its parent is reported.
func attestationJSONErrorMessage(err error) string {
	var syntaxErr *jsontext.SyntacticError
	var semanticErr *jsonv2.SemanticError
	switch {
	case errors.As(err, &syntaxErr):
		if errors.Is(err, jsontext.ErrDuplicateName) {
			return errMsgInvalidBodyPrefix + ": repeated member in " + jsonLocation(syntaxErr.JSONPointer.Parent())
		}
		return errMsgInvalidBodyPrefix + ": must be one JSON object of valid UTF-8 text with paired \\u surrogate escapes"
	case errors.As(err, &semanticErr):
		location := jsonLocation(semanticErr.JSONPointer)
		switch {
		case semanticErr.JSONPointer == "":
			return errMsgInvalidBodyPrefix + ": must be a JSON object"
		case errors.Is(err, jsonv2.ErrUnknownName):
			return errMsgInvalidBodyPrefix + ": unknown member in " + jsonLocation(semanticErr.JSONPointer.Parent())
		case errors.Is(err, errAttestationNullValue):
			return errMsgInvalidBodyPrefix + ": " + location + " must not be null"
		}
		return errMsgInvalidBodyPrefix + ": " + location + " has an invalid value"
	}
	return errMsgInvalidBodyPrefix
}

// jsonLocation names a JSON pointer for messages.
func jsonLocation(pointer jsontext.Pointer) string {
	if pointer == "" {
		return "the top-level object"
	}
	return string(pointer)
}

// bodyReadError preserves Echo's body-limit error and reports any other read
// failure as a malformed body.
func bodyReadError(err error) error {
	var httpErr *echo.HTTPError
	if errors.As(err, &httpErr) {
		return httpErr
	}
	return cutil.NewAPIError(http.StatusBadRequest, "Failed to read request body", nil)
}
