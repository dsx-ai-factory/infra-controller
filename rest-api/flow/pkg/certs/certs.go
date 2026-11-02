// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package certs provides TLS configuration building from explicit certificate
// file paths. It has no environment or deployment assumptions — callers supply
// all paths directly.
package certs

import (
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"os"

	dynamictls "github.com/NVIDIA/infra-controller/rest-api/common/pkg/tls"
)

// Config holds explicit file paths for the CA cert, TLS cert, and TLS key.
// The same cert/key pair is used for both client and server roles in mTLS.
type Config struct {
	CACert  string // path to CA certificate file
	TLSCert string // path to TLS certificate file
	TLSKey  string // path to TLS private key file
}

// IsSet reports whether all three certificate paths are set.
func (c Config) IsSet() bool {
	return c.CACert != "" && c.TLSCert != "" && c.TLSKey != ""
}

// Validate checks that all three certificate paths are set.
// Either all must be non-empty or none — partial configuration is an error.
func (c Config) Validate() error {
	set := 0
	if c.CACert != "" {
		set++
	}
	if c.TLSCert != "" {
		set++
	}
	if c.TLSKey != "" {
		set++
	}

	if set != 0 && set != 3 {
		return errors.New("ca-cert, tls-cert, and tls-key must all be provided together")
	}

	return nil
}

// loadCerts reads the CA cert pool and the cert/key pair from the file paths in c.
func (c Config) loadCerts() (*x509.CertPool, tls.Certificate, error) {
	caCert, err := os.ReadFile(c.CACert)
	if err != nil {
		return nil, tls.Certificate{}, fmt.Errorf("failed to read CA cert %q: %w", c.CACert, err)
	}

	certPool := x509.NewCertPool()
	if !certPool.AppendCertsFromPEM(caCert) {
		return nil, tls.Certificate{}, fmt.Errorf("failed to parse CA cert %q", c.CACert)
	}

	cert, err := tls.LoadX509KeyPair(c.TLSCert, c.TLSKey)
	if err != nil {
		return nil, tls.Certificate{}, fmt.Errorf("failed to load cert/key (%q, %q): %w", c.TLSCert, c.TLSKey, err)
	}

	return certPool, cert, nil
}

// TLSConfig builds a client-side tls.Config from the explicit file paths in c.
// RootCAs is set to verify the server certificate. GetClientCertificate is used
// instead of Certificates to ensure the client always presents its certificate
// during the TLS handshake. With the Certificates field, Go's TLS stack only
// selects a certificate if its issuer matches the acceptable CA list sent by
// the server in its CertificateRequest message. When no match is found, Go
// silently sends an empty certificate list, causing the server to reject the
// connection with "certificate required". GetClientCertificate bypasses this
// matching and unconditionally returns the certificate, leaving verification to
// the server.
func (c Config) TLSConfig(serverName string) (*tls.Config, error) {
	certPool, cert, err := c.loadCerts()
	if err != nil {
		return nil, err
	}

	return &tls.Config{
		MinVersion: tls.VersionTLS12,
		GetClientCertificate: func(*tls.CertificateRequestInfo) (*tls.Certificate, error) {
			return &cert, nil
		},
		RootCAs:    certPool,
		ServerName: serverName,
	}, nil
}

// ServerTLSConfig builds a server-side tls.Config from the explicit file paths
// in c. Certificates is set so the server can present its certificate during
// the TLS handshake. ClientAuth and ClientCAs are set to require and verify the
// client certificate.
func (c Config) ServerTLSConfig() (*tls.Config, error) {
	certPool, cert, err := c.loadCerts()
	if err != nil {
		return nil, err
	}

	return &tls.Config{
		MinVersion:   tls.VersionTLS12,
		Certificates: []tls.Certificate{cert},
		ClientAuth:   tls.RequireAndVerifyClientCert,
		ClientCAs:    certPool,
	}, nil
}

// DynamicTLSConfig builds a client-side TLS config whose certificate and key
// are refreshed from disk. The caller owns the returned dynamic config and
// must close it when the client is no longer used.
func (c Config) DynamicTLSConfig(serverName string) (*tls.Config, *dynamictls.DynTLSCfg, error) {
	err := c.Validate()
	if err != nil {
		return nil, nil, err
	}
	dynamicConfig, err := dynamictls.NewDynTLSCfg(c.TLSKey, c.TLSCert, c.CACert)
	if err != nil {
		return nil, nil, err
	}

	baseConfig := &tls.Config{
		MinVersion: tls.VersionTLS12,
		ServerName: serverName,
	}

	return dynamicConfig.WithTLSCfg(baseConfig).ClientCfg(), dynamicConfig, nil
}

// DynamicServerTLSConfig builds a server-side TLS config whose certificate,
// key, and client CA pool are refreshed from disk. The caller owns the returned
// dynamic config and must close it when the server stops.
func (c Config) DynamicServerTLSConfig() (*tls.Config, *dynamictls.DynTLSCfg, error) {
	err := c.Validate()
	if err != nil {
		return nil, nil, err
	}
	dynamicConfig, err := dynamictls.NewDynTLSCfg(c.TLSKey, c.TLSCert, c.CACert)
	if err != nil {
		return nil, nil, err
	}

	baseConfig := &tls.Config{
		MinVersion: tls.VersionTLS12,
		ClientAuth: tls.RequireAndVerifyClientCert,
	}

	return dynamicConfig.WithTLSCfg(baseConfig).ServerCfg(), dynamicConfig, nil
}
