// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"os"
)

// ErrNotPresent is returned when certificates are not found in the expected directory.
var ErrNotPresent = errors.New("certificates are not present")

// TLSConfig loads TLS certificates from the directory specified by CERTDIR environment variable.
// If CERTDIR is not set, defaults to /var/run/secrets/spiffe.io (standard k8s SPIFFE path).
//
// Expected files in the cert directory:
//   - ca.crt: CA certificate for client verification
//   - tls.crt: Server certificate
//   - tls.key: Server private key
//
// Returns ErrNotPresent if the CA cert file doesn't exist, allowing callers to fall back to plaintext.
// Returns a different error if certs exist but are invalid.
func TLSConfig() (tlsConfig *tls.Config, certDir string, err error) {
	certDir = os.Getenv("CERTDIR")
	if certDir == "" {
		// Cert directory in k8s
		certDir = "/var/run/secrets/spiffe.io"
	}

	caCert, err := os.ReadFile(certDir + "/ca.crt")
	if err != nil {
		return nil, certDir, ErrNotPresent
	}

	serverCert, err := tls.LoadX509KeyPair(certDir+"/tls.crt", certDir+"/tls.key")
	if err != nil {
		return nil, certDir, fmt.Errorf("invalid certs present: %w", err)
	}

	certPool := x509.NewCertPool()
	if !certPool.AppendCertsFromPEM(caCert) {
		return nil, certDir, fmt.Errorf("invalid CA cert present")
	}

	return &tls.Config{
		Certificates: []tls.Certificate{serverCert},
		RootCAs:      certPool,
		ClientAuth:   tls.RequireAndVerifyClientCert,
		ClientCAs:    certPool,
	}, certDir, nil
}
