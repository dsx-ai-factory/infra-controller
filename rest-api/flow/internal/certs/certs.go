// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package certs provides TLS configuration resolution using deployment-specific
// defaults: the CERTDIR environment variable and the Kubernetes SPIFFE secret
// path. For explicit path-based loading, use pkg/certs.
package certs

import (
	"crypto/tls"
	"errors"
	"fmt"
	"os"
	"path/filepath"

	dynamictls "github.com/NVIDIA/infra-controller/rest-api/common/pkg/tls"
	pkgcerts "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/certs"
)

// Default certificate directory and file names for the Kubernetes SPIFFE
// workload identity secret mount.
const (
	defaultCertDir  = "/var/run/secrets/spiffe.io"
	defaultCACert   = "ca.crt"
	defaultCertFile = "tls.crt"
	defaultKeyFile  = "tls.key"
)

// ErrNotPresent is returned when no certificate files are found at the
// resolved directory. Callers may use errors.Is(err, ErrNotPresent) to
// detect this case and fall back to non-mTLS.
var ErrNotPresent = errors.New("certificates are not present")

// IsTLSAvailable reports whether TLS certificates can be resolved. It checks,
// in order: explicit paths in c, the CERTDIR env var, and the k8s SPIFFE
// default directory. This mirrors the resolution order used by ResolveDynamicServer
// without loading any files.
func IsTLSAvailable(c pkgcerts.Config) bool {
	if c.IsSet() {
		for _, path := range []string{c.CACert, c.TLSCert, c.TLSKey} {
			if _, err := os.Stat(path); err != nil {
				return false
			}
		}
		return true
	}

	certDir := os.Getenv("CERTDIR")
	if certDir == "" {
		certDir = defaultCertDir
	}

	for _, name := range []string{defaultCACert, defaultCertFile, defaultKeyFile} {
		if _, err := os.Stat(filepath.Join(certDir, name)); err != nil {
			return false
		}
	}

	return true
}

// ResolveDynamicServer returns a periodically refreshed server-side TLS config,
// its source description, and the refresh lifecycle owned by the caller.
func ResolveDynamicServer(
	c pkgcerts.Config,
) (*tls.Config, string, *dynamictls.DynTLSCfg, error) {
	if err := c.Validate(); err != nil {
		return nil, "", nil, err
	}

	if c.IsSet() {
		tlsConfig, dynamicConfig, err := c.DynamicServerTLSConfig()
		return tlsConfig, c.CACert, dynamicConfig, err
	}

	return dynamicTLSConfigFromDir(
		func(c pkgcerts.Config) (*tls.Config, *dynamictls.DynTLSCfg, error) {
			return c.DynamicServerTLSConfig()
		},
	)
}

// DynamicTLSConfig resolves the deployment certificate paths and returns a
// periodically refreshed client-side TLS config. The caller owns the returned
// refresh lifecycle.
func DynamicTLSConfig() (*tls.Config, string, *dynamictls.DynTLSCfg, error) {
	return dynamicTLSConfigFromDir(
		func(c pkgcerts.Config) (*tls.Config, *dynamictls.DynTLSCfg, error) {
			return c.DynamicTLSConfig("")
		},
	)
}

func dynamicTLSConfigFromDir(
	build func(pkgcerts.Config) (*tls.Config, *dynamictls.DynTLSCfg, error),
) (*tls.Config, string, *dynamictls.DynTLSCfg, error) {
	certDir := os.Getenv("CERTDIR")
	if certDir == "" {
		certDir = defaultCertDir
	}

	tlsConfig, dynamicConfig, err := build(
		pkgcerts.Config{
			CACert:  filepath.Join(certDir, defaultCACert),
			TLSCert: filepath.Join(certDir, defaultCertFile),
			TLSKey:  filepath.Join(certDir, defaultKeyFile),
		},
	)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, certDir, nil, ErrNotPresent
		}
		return nil, certDir, nil, fmt.Errorf("loading certs from %q: %w", certDir, err)
	}

	return tlsConfig, certDir, dynamicConfig, nil
}
