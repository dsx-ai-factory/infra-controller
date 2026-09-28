// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package temporal

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"math/big"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/endpoint"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// writeCertDir generates a self-signed CA and client cert/key, writing them
// into dir using the file names expected by buildTLSConfig.
func writeCertDir(t *testing.T, dir string) {
	t.Helper()

	caKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	require.NoError(t, err)

	caTemplate := &x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "Test CA"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		IsCA:         true,
		KeyUsage:     x509.KeyUsageCertSign,
	}
	caDER, err := x509.CreateCertificate(rand.Reader, caTemplate, caTemplate, &caKey.PublicKey, caKey)
	require.NoError(t, err)

	clientKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	require.NoError(t, err)

	clientTemplate := &x509.Certificate{
		SerialNumber: big.NewInt(2),
		Subject:      pkix.Name{CommonName: "Test Client"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		KeyUsage:     x509.KeyUsageDigitalSignature,
	}
	clientDER, err := x509.CreateCertificate(rand.Reader, clientTemplate, caTemplate, &clientKey.PublicKey, caKey)
	require.NoError(t, err)

	clientKeyDER, err := x509.MarshalECPrivateKey(clientKey)
	require.NoError(t, err)

	writePEM(t, filepath.Join(dir, caCertificateFileName), "CERTIFICATE", caDER)
	writePEM(t, filepath.Join(dir, clientCertificateFileName), "CERTIFICATE", clientDER)
	writePEM(t, filepath.Join(dir, clientKeyFileName), "EC PRIVATE KEY", clientKeyDER)
}

func writePEM(t *testing.T, path, pemType string, der []byte) {
	t.Helper()
	f, err := os.Create(path)
	require.NoError(t, err)
	defer f.Close()
	require.NoError(t, pem.Encode(f, &pem.Block{Type: pemType, Bytes: der}))
}

func TestBuildTLSConfig(t *testing.T) {
	t.Run("TLS disabled returns nil", func(t *testing.T) {
		cfg := Config{EnableTLS: false}
		tlsConfig, dynamicConfig, err := buildTLSConfig(cfg)
		require.NoError(t, err)
		assert.Nil(t, tlsConfig)
		assert.Nil(t, dynamicConfig)
	})

	t.Run("TLS enabled with valid cert dir", func(t *testing.T) {
		dir := t.TempDir()
		writeCertDir(t, dir)

		cfg := Config{
			EnableTLS:  true,
			ServerName: "temporal.example.com",
			Endpoint:   endpoint.Config{CACertificatePath: dir},
		}
		tlsConfig, dynamicConfig, err := buildTLSConfig(cfg)
		require.NoError(t, err)
		defer dynamicConfig.Close()
		assert.NotNil(t, tlsConfig)
		assert.NotNil(t, tlsConfig.RootCAs)
		assert.NotNil(t, tlsConfig.GetClientCertificate)
		assert.Equal(t, "temporal.example.com", tlsConfig.ServerName)
	})

	t.Run("TLS enabled with trailing slash in path", func(t *testing.T) {
		dir := t.TempDir()
		writeCertDir(t, dir)

		cfg := Config{
			EnableTLS:  true,
			ServerName: "temporal.example.com",
			Endpoint:   endpoint.Config{CACertificatePath: dir + "/"},
		}
		tlsConfig, dynamicConfig, err := buildTLSConfig(cfg)
		require.NoError(t, err)
		defer dynamicConfig.Close()
		assert.NotNil(t, tlsConfig)
		assert.Equal(t, "temporal.example.com", tlsConfig.ServerName)
	})

	t.Run("TLS enabled with missing cert files returns error", func(t *testing.T) {
		dir := t.TempDir()
		// dir exists but contains no cert files

		cfg := Config{
			EnableTLS:  true,
			ServerName: "temporal.example.com",
			Endpoint:   endpoint.Config{CACertificatePath: dir},
		}
		_, _, err := buildTLSConfig(cfg)
		require.Error(t, err)
	})
}
