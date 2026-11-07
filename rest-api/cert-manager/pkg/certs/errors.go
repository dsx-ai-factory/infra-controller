// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"fmt"
	"net/http"
)

// Error defines the error return codes
type Error int

// Error codes
const (
	ErrorNone Error = iota

	ErrorParseRequest
	ErrorBadAPIVersion
	ErrorBadOwnerInfo
	ErrorVerifyCertificateRequest
	ErrorGetCertificate
	ErrorEncryptCertificatePrivateKey
	ErrorMarshalJSON

	ErrorRequestCACertificate
	ErrorDecodeCACertificate
	ErrorBadPKIRequest

	ErrorEventLogParse
	ErrorEventLogVerification
)

type errorInfo struct {
	str      string
	httpCode int
}

var errorInfoTable = [...]errorInfo{
	{"ErrorNone", http.StatusOK},
	{"ErrorParseRequest", http.StatusBadRequest},
	{"ErrorBadAPIVersion", http.StatusBadRequest},
	{"ErrorBadOwnerInfo", http.StatusBadRequest},
	{"ErrorVerifyCertificateRequest", http.StatusUnauthorized},
	{"ErrorGetCertificate", http.StatusInternalServerError},
	{"ErrorEncryptCertificatePrivateKey", http.StatusInternalServerError},
	{"ErrorMarshalJSON", http.StatusInternalServerError},
	{"ErrorInvalidNonce", http.StatusBadRequest},
	{"ErrorExpiredNonce", http.StatusBadRequest},
	{"ErrorRequestCACertificate", http.StatusInternalServerError},
	{"ErrorDecodeCACertificate", http.StatusInternalServerError},
	{"ErrorBadPKIRequest", http.StatusBadRequest},
	{"ErrorEventLogParse", http.StatusBadRequest},
	{"ErrorEventLogVerification", http.StatusBadRequest},
}

// Error returns a go error
func (e Error) Error() string {
	if e == ErrorNone {
		return ""
	}
	return fmt.Sprintf("CredsManager error, internal code %d", e)
}

// String returns the error in string form
func (e Error) String() string {
	return errorInfoTable[e].str
}

// Code returns the error in int form
func (e Error) Code() int {
	return errorInfoTable[e].httpCode
}
