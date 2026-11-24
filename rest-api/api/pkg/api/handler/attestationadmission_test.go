// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// tooLargeReader fails like Echo's body-limit reader.
type tooLargeReader struct{}

func (tooLargeReader) Read([]byte) (int, error) { return 0, echo.ErrStatusRequestEntityTooLarge }

func newAdmissionContext(method, target, contentType string, body io.Reader) echo.Context {
	req := httptest.NewRequest(method, target, body)
	if contentType != "" {
		req.Header.Set(echo.HeaderContentType, contentType)
	}
	return echo.New().NewContext(req, httptest.NewRecorder())
}

// admissionStatus returns the status an admission error renders as.
func admissionStatus(t *testing.T, err error) int {
	t.Helper()
	var apiErr *cutil.APIError
	if errors.As(err, &apiErr) {
		return apiErr.Code
	}
	var httpErr *echo.HTTPError
	require.ErrorAs(t, err, &httpErr)
	return httpErr.Code
}

func TestAdmitAttestationQuery(t *testing.T) {
	var deleteReq model.APIAttestationProfileDeleteRequest
	cases := []struct {
		name     string
		rawQuery string
		dtos     []any
		wantErr  string
	}{
		{name: "known keys", rawQuery: "siteId=a&ifVersionMatch=V1", dtos: []any{&deleteReq}},
		{name: "no query on bodyless route", rawQuery: "", dtos: nil},
		{name: "unknown key", rawQuery: "siteId=a&force=true", dtos: []any{&deleteReq}, wantErr: "unknown query parameter"},
		{name: "query on route without query", rawQuery: "siteId=a", dtos: nil, wantErr: "unknown query parameter"},
		{name: "repeated identical key", rawQuery: "siteId=a&siteId=a", dtos: []any{&deleteReq}, wantErr: "at most once"},
		{name: "percent-encoded alias repeats key", rawQuery: "siteId=a&%73iteId=b", dtos: []any{&deleteReq}, wantErr: "at most once"},
		{name: "invalid UTF-8 value", rawQuery: "ifVersionMatch=%FF", dtos: []any{&deleteReq}, wantErr: "valid UTF-8"},
		{name: "invalid UTF-8 key", rawQuery: "%FF=1", dtos: []any{&deleteReq}, wantErr: "valid UTF-8"},
		{name: "malformed escape", rawQuery: "siteId=%zz", dtos: []any{&deleteReq}, wantErr: "malformed"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			c := newAdmissionContext(http.MethodGet, "/?"+tc.rawQuery, "", nil)
			apiErr := admitAttestationQuery(c, tc.dtos...)
			if tc.wantErr == "" {
				require.Nil(t, apiErr)
				return
			}
			require.NotNil(t, apiErr)
			assert.Equal(t, http.StatusBadRequest, apiErr.Code)
			assert.Contains(t, apiErr.Message, tc.wantErr)
		})
	}
}

func TestDecodeClosedJSONObject(t *testing.T) {
	cases := []struct {
		name        string
		contentType string
		body        io.Reader
		wantStatus  int
		wantMessage string
		check       func(*testing.T, model.APIAttestationProfileUpdateRequest)
	}{
		{
			name:        "accepts exact members",
			contentType: "application/json; charset=utf-8",
			body:        strings.NewReader(`{"siteId":"s","selection":{"mode":"Allowlist","componentIds":[{"exact":"A","prefix":null}]},"ifVersionMatch":"V1"}`),
			check: func(t *testing.T, r model.APIAttestationProfileUpdateRequest) {
				require.NotNil(t, r.Selection)
				assert.Equal(t, "A", *r.Selection.ComponentIDs[0].Exact)
				assert.False(t, r.AcknowledgeFallbackImpact)
			},
		},
		{
			name:        "preserves escaped replacement character and literal backslash u",
			contentType: "application/json",
			body:        strings.NewReader(`{"ifVersionMatch":"�\\u0000😀"}`),
			check: func(t *testing.T, r model.APIAttestationProfileUpdateRequest) {
				assert.Equal(t, "�\\u0000\U0001F600", *r.IfVersionMatch)
			},
		},
		{name: "missing media type", body: strings.NewReader(`{}`), wantStatus: http.StatusUnsupportedMediaType},
		{name: "wrong media type", contentType: "text/plain", body: strings.NewReader(`{}`), wantStatus: http.StatusUnsupportedMediaType},
		{name: "oversized body keeps 413", contentType: "application/json", body: tooLargeReader{}, wantStatus: http.StatusRequestEntityTooLarge},
		{name: "invalid UTF-8", contentType: "application/json", body: strings.NewReader("{\"ifVersionMatch\":\"\xff\"}"), wantStatus: http.StatusBadRequest, wantMessage: "UTF-8"},
		{name: "unpaired high surrogate", contentType: "application/json", body: strings.NewReader(`{"ifVersionMatch":"\uD800x"}`), wantStatus: http.StatusBadRequest, wantMessage: "surrogate"},
		{name: "lone low surrogate", contentType: "application/json", body: strings.NewReader(`{"ifVersionMatch":"\uDC00"}`), wantStatus: http.StatusBadRequest, wantMessage: "surrogate"},
		{name: "surrogate in member name", contentType: "application/json", body: strings.NewReader(`{"\uD800":1}`), wantStatus: http.StatusBadRequest, wantMessage: "surrogate"},
		{name: "malformed JSON", contentType: "application/json", body: strings.NewReader(`{"siteId":`), wantStatus: http.StatusBadRequest, wantMessage: "one JSON object"},
		{name: "top-level array", contentType: "application/json", body: strings.NewReader(`[]`), wantStatus: http.StatusBadRequest, wantMessage: "must be a JSON object"},
		{name: "top-level null", contentType: "application/json", body: strings.NewReader(`null`), wantStatus: http.StatusBadRequest, wantMessage: "must be a JSON object"},
		{name: "empty body", contentType: "application/json", body: strings.NewReader(``), wantStatus: http.StatusBadRequest, wantMessage: "one JSON object"},
		{name: "trailing value", contentType: "application/json", body: strings.NewReader(`{} {}`), wantStatus: http.StatusBadRequest},
		{name: "unknown member", contentType: "application/json", body: strings.NewReader(`{"hardwareClass":"a_b"}`), wantStatus: http.StatusBadRequest, wantMessage: "unknown member in the top-level object"},
		{name: "case-folded member", contentType: "application/json", body: strings.NewReader(`{"SiteId":"s"}`), wantStatus: http.StatusBadRequest, wantMessage: "unknown member in the top-level object"},
		{name: "repeated member", contentType: "application/json", body: strings.NewReader(`{"siteId":"a","siteId":"b"}`), wantStatus: http.StatusBadRequest, wantMessage: "repeated"},
		{name: "repeated member spelled with escape", contentType: "application/json", body: strings.NewReader(`{"siteId":"a","siteId":"b"}`), wantStatus: http.StatusBadRequest, wantMessage: "repeated"},
		{name: "unknown nested member", contentType: "application/json", body: strings.NewReader(`{"selection":{"mode":"All","regex":null}}`), wantStatus: http.StatusBadRequest, wantMessage: "unknown member in /selection"},
		{name: "repeated nested member", contentType: "application/json", body: strings.NewReader(`{"selection":{"mode":"All","mode":"None"}}`), wantStatus: http.StatusBadRequest, wantMessage: "repeated member in /selection"},
		{name: "null for non-nullable member", contentType: "application/json", body: strings.NewReader(`{"acknowledgeFallbackImpact":null}`), wantStatus: http.StatusBadRequest, wantMessage: "/acknowledgeFallbackImpact must not be null"},
		{name: "null list element", contentType: "application/json", body: strings.NewReader(`{"selection":{"mode":"Allowlist","componentIds":[null]}}`), wantStatus: http.StatusBadRequest, wantMessage: "/selection/componentIds/0 must not be null"},
		{name: "null for nullable members", contentType: "application/json", body: strings.NewReader(`{"selection":null,"ifVersionMatch":null}`),
			check: func(t *testing.T, r model.APIAttestationProfileUpdateRequest) {
				assert.Nil(t, r.Selection)
				assert.Nil(t, r.IfVersionMatch)
			}},
		{name: "wrong scalar type", contentType: "application/json", body: strings.NewReader(`{"siteId":1}`), wantStatus: http.StatusBadRequest, wantMessage: "/siteId has an invalid value"},
		{name: "path field is not a member", contentType: "application/json", body: strings.NewReader(`{"HardwareClass":"a_b"}`), wantStatus: http.StatusBadRequest, wantMessage: "unknown member"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			c := newAdmissionContext(http.MethodPatch, "/", tc.contentType, tc.body)
			var dst model.APIAttestationProfileUpdateRequest
			err := decodeClosedJSONObject(c, &dst)
			if tc.wantStatus == 0 {
				require.NoError(t, err)
				tc.check(t, dst)
				return
			}
			require.Error(t, err)
			assert.Equal(t, tc.wantStatus, admissionStatus(t, err))
			if tc.wantMessage != "" {
				assert.Contains(t, err.Error(), tc.wantMessage)
			}
			for _, echoed := range []string{"\\uD800", "\\uDC00", "SiteId", "hardwareClass", "regex", "None\""} {
				assert.NotContains(t, err.Error(), echoed, "error must not echo request content")
			}
		})
	}
}

func TestRequireEmptyAttestationBody(t *testing.T) {
	cases := []struct {
		name       string
		body       io.Reader
		wantStatus int
	}{
		{name: "no body", body: nil},
		{name: "whitespace", body: strings.NewReader(" "), wantStatus: http.StatusBadRequest},
		{name: "empty object", body: strings.NewReader("{}"), wantStatus: http.StatusBadRequest},
		{name: "oversized", body: tooLargeReader{}, wantStatus: http.StatusRequestEntityTooLarge},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			c := newAdmissionContext(http.MethodDelete, "/", "", tc.body)
			err := requireEmptyAttestationBody(c)
			if tc.wantStatus == 0 {
				require.NoError(t, err)
				return
			}
			assert.Equal(t, tc.wantStatus, admissionStatus(t, err))
		})
	}
}

func TestAttestationMachineID(t *testing.T) {
	cases := []struct {
		name    string
		path    string
		want    string
		wantErr bool
	}{
		{name: "plain", path: "/machine/fm100abc/attestation", want: "fm100abc"},
		{name: "escaped percent stays literal", path: "/machine/a%2525b/attestation", want: "a%25b"},
		{name: "lowercase escape of unreserved character", path: "/machine/a%2db/attestation", want: "a-b"},
		{name: "escaped space", path: "/machine/a%20b/attestation", want: "a b"},
		{name: "escaped invalid UTF-8 is decoded for validation", path: "/machine/%FF/attestation", want: "\xff"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			e := echo.New()
			var got string
			var gotErr *cutil.APIError
			e.POST("/machine/:id/attestation", func(c echo.Context) error {
				got, gotErr = attestationMachineID(c)
				return nil
			})
			rec := httptest.NewRecorder()
			e.ServeHTTP(rec, httptest.NewRequest(http.MethodPost, tc.path, nil))
			if tc.wantErr {
				require.NotNil(t, gotErr)
				return
			}
			require.Nil(t, gotErr)
			assert.Equal(t, tc.want, got)
		})
	}
}
