// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"context"
	"encoding/pem"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
)

type caCertificateIssuerStub struct {
	CertificateIssuer
	certificate string
	err         error
}

func (s caCertificateIssuerStub) GetCACertificate(context.Context) (string, error) {
	return s.certificate, s.err
}

func TestPKICACertificateHandler_ServeHTTP(t *testing.T) {
	// Include invalid UTF-8 bytes to detect accidental text conversion.
	der := []byte{0x30, 0x82, 0x01, 0xff, 0x00, 0x80}
	certificate := string(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}))
	tests := []struct {
		name        string
		path        string
		certificate string
		issuerErr   error
		contentType string
		status      int
		body        []byte
	}{
		{
			name:        "DER success",
			path:        "/v1/pki/ca",
			certificate: certificate,
			contentType: "application/pkix-cert",
			status:      http.StatusOK,
			body:        der,
		},
		{
			name:        "PEM success",
			path:        "/v1/pki/ca/pem",
			certificate: certificate,
			contentType: "text/plain; charset=utf-8",
			status:      http.StatusOK,
			body:        []byte(certificate),
		},
		{
			name:        "issuer failure",
			path:        "/v1/pki/ca",
			issuerErr:   errors.New("issuer unavailable"),
			contentType: "text/plain; charset=utf-8",
			status:      ErrorRequestCACertificate.Code(),
			body:        []byte(ErrorRequestCACertificate.Error()),
		},
		{
			name:        "decode failure",
			path:        "/v1/pki/ca",
			certificate: "invalid PEM",
			contentType: "text/plain; charset=utf-8",
			status:      ErrorDecodeCACertificate.Code(),
			body:        []byte(ErrorDecodeCACertificate.Error()),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			handler := &pkiCACertificateHandler{certificateIssuer: caCertificateIssuerStub{
				certificate: tt.certificate,
				err:         tt.issuerErr,
			}}
			response := httptest.NewRecorder()
			request := httptest.NewRequest(http.MethodGet, tt.path, nil)

			handler.ServeHTTP(response, request)

			assert.Equal(t, tt.status, response.Code)
			assert.Equal(t, tt.contentType, response.Header().Get("Content-Type"))
			assert.Equal(t, tt.body, response.Body.Bytes())
		})
	}
}
