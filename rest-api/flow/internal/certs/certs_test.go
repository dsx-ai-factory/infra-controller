// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"math/big"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	pkgcerts "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/certs"
)

// generateTestCerts creates a self-signed CA and a client cert/key in a temp
// directory using the standard file names (ca.crt, tls.crt, tls.key).
// Returns the directory path.
func generateTestCerts(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()

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

	writePEM(t, filepath.Join(dir, defaultCACert), "CERTIFICATE", caDER)
	writePEM(t, filepath.Join(dir, defaultCertFile), "CERTIFICATE", clientDER)
	writePEM(t, filepath.Join(dir, defaultKeyFile), "EC PRIVATE KEY", clientKeyDER)

	return dir
}

func writePEM(t *testing.T, path, pemType string, der []byte) {
	t.Helper()
	f, err := os.Create(path)
	require.NoError(t, err)
	defer f.Close()
	require.NoError(t, pem.Encode(f, &pem.Block{Type: pemType, Bytes: der}))
}

func TestDynamicTLSConfig(t *testing.T) {
	for _, name := range []string{"valid CERTDIR", "missing files", "default directory"} {
		t.Run(name, func(t *testing.T) {
			dir := t.TempDir()
			if name == "valid CERTDIR" {
				dir = generateTestCerts(t)
			}
			if name == "default directory" {
				dir = ""
			}
			t.Setenv("CERTDIR", dir)
			cfg, source, dynamic, err := DynamicTLSConfig()
			if dynamic != nil {
				defer dynamic.Close()
			}
			if name != "valid CERTDIR" {
				require.ErrorIs(t, err, ErrNotPresent)
				assert.Nil(t, cfg)
				if name == "default directory" {
					assert.Equal(t, defaultCertDir, source)
				}
				return
			}
			require.NoError(t, err)
			assert.Equal(t, dir, source)
			assert.NotNil(t, cfg.GetClientCertificate)
		})
	}
}

func TestIsTLSAvailable(t *testing.T) {
	// stubCerts writes empty stub files for each provided name in a new TempDir
	// and returns the directory path. Names not in the list are absent.
	stubCerts := func(t *testing.T, names ...string) string {
		t.Helper()
		dir := t.TempDir()
		for _, name := range names {
			require.NoError(t, os.WriteFile(filepath.Join(dir, name), []byte("stub"), 0600))
		}
		return dir
	}

	t.Run("explicit paths: all three files present", func(t *testing.T) {
		dir := stubCerts(t, defaultCACert, defaultCertFile, defaultKeyFile)
		c := pkgcerts.Config{
			CACert:  filepath.Join(dir, defaultCACert),
			TLSCert: filepath.Join(dir, defaultCertFile),
			TLSKey:  filepath.Join(dir, defaultKeyFile),
		}
		assert.True(t, IsTLSAvailable(c))
	})

	t.Run("explicit paths: ca.crt missing", func(t *testing.T) {
		dir := stubCerts(t, defaultCertFile, defaultKeyFile)
		c := pkgcerts.Config{
			CACert:  filepath.Join(dir, defaultCACert),
			TLSCert: filepath.Join(dir, defaultCertFile),
			TLSKey:  filepath.Join(dir, defaultKeyFile),
		}
		assert.False(t, IsTLSAvailable(c))
	})

	t.Run("explicit paths: tls.crt missing", func(t *testing.T) {
		dir := stubCerts(t, defaultCACert, defaultKeyFile)
		c := pkgcerts.Config{
			CACert:  filepath.Join(dir, defaultCACert),
			TLSCert: filepath.Join(dir, defaultCertFile),
			TLSKey:  filepath.Join(dir, defaultKeyFile),
		}
		assert.False(t, IsTLSAvailable(c))
	})

	t.Run("explicit paths: tls.key missing", func(t *testing.T) {
		dir := stubCerts(t, defaultCACert, defaultCertFile)
		c := pkgcerts.Config{
			CACert:  filepath.Join(dir, defaultCACert),
			TLSCert: filepath.Join(dir, defaultCertFile),
			TLSKey:  filepath.Join(dir, defaultKeyFile),
		}
		assert.False(t, IsTLSAvailable(c))
	})

	t.Run("explicit paths take precedence over CERTDIR", func(t *testing.T) {
		badDir := stubCerts(t, defaultCACert, defaultCertFile) // tls.key absent
		goodDir := stubCerts(t, defaultCACert, defaultCertFile, defaultKeyFile)
		t.Setenv("CERTDIR", goodDir)

		c := pkgcerts.Config{
			CACert:  filepath.Join(badDir, defaultCACert),
			TLSCert: filepath.Join(badDir, defaultCertFile),
			TLSKey:  filepath.Join(badDir, defaultKeyFile),
		}
		assert.False(t, IsTLSAvailable(c))
	})

	t.Run("CERTDIR: all three files present", func(t *testing.T) {
		dir := stubCerts(t, defaultCACert, defaultCertFile, defaultKeyFile)
		t.Setenv("CERTDIR", dir)
		assert.True(t, IsTLSAvailable(pkgcerts.Config{}))
	})

	t.Run("CERTDIR: ca.crt missing", func(t *testing.T) {
		dir := stubCerts(t, defaultCertFile, defaultKeyFile)
		t.Setenv("CERTDIR", dir)
		assert.False(t, IsTLSAvailable(pkgcerts.Config{}))
	})

	t.Run("CERTDIR: tls.crt missing", func(t *testing.T) {
		dir := stubCerts(t, defaultCACert, defaultKeyFile)
		t.Setenv("CERTDIR", dir)
		assert.False(t, IsTLSAvailable(pkgcerts.Config{}))
	})

	t.Run("CERTDIR: tls.key missing", func(t *testing.T) {
		dir := stubCerts(t, defaultCACert, defaultCertFile)
		t.Setenv("CERTDIR", dir)
		assert.False(t, IsTLSAvailable(pkgcerts.Config{}))
	})

	t.Run("CERTDIR: empty dir", func(t *testing.T) {
		t.Setenv("CERTDIR", t.TempDir())
		assert.False(t, IsTLSAvailable(pkgcerts.Config{}))
	})
}

func TestResolveDynamicServer(t *testing.T) {
	for _, name := range []string{"explicit paths", "CERTDIR fallback", "missing files", "partial configuration"} {
		t.Run(name, func(t *testing.T) {
			dir := generateTestCerts(t)
			t.Setenv("CERTDIR", dir)
			c := pkgcerts.Config{}
			if name == "explicit paths" {
				c = pkgcerts.Config{CACert: filepath.Join(dir, defaultCACert), TLSCert: filepath.Join(dir, defaultCertFile), TLSKey: filepath.Join(dir, defaultKeyFile)}
				t.Setenv("CERTDIR", t.TempDir())
			}
			if name == "missing files" {
				t.Setenv("CERTDIR", t.TempDir())
			}
			if name == "partial configuration" {
				c.CACert = "ca.crt"
			}
			cfg, source, dynamic, err := ResolveDynamicServer(c)
			if dynamic != nil {
				defer dynamic.Close()
			}
			if name == "missing files" {
				require.ErrorIs(t, err, ErrNotPresent)
				return
			}
			if name == "partial configuration" {
				require.EqualError(t, err, "ca-cert, tls-cert, and tls-key must all be provided together")
				return
			}
			require.NoError(t, err)
			if name == "explicit paths" {
				assert.Equal(t, c.CACert, source)
			} else {
				assert.Equal(t, dir, source)
			}
			current, err := cfg.GetConfigForClient(nil)
			require.NoError(t, err)
			assert.NotEmpty(t, current.Certificates)
			assert.Equal(t, tls.RequireAndVerifyClientCert, current.ClientAuth)
			assert.NotNil(t, current.ClientCAs)
		})
	}
}
