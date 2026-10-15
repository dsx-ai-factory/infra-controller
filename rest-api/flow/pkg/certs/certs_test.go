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
)

// generateTestCerts creates a self-signed CA and a client cert/key in a temp
// directory, returning paths to ca.crt, tls.crt, and tls.key.
func generateTestCerts(t *testing.T) (caFile, certFile, keyFile string) {
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

	caFile = filepath.Join(dir, "ca.crt")
	writePEM(t, caFile, "CERTIFICATE", caDER)

	certFile = filepath.Join(dir, "tls.crt")
	writePEM(t, certFile, "CERTIFICATE", clientDER)

	keyFile = filepath.Join(dir, "tls.key")
	writePEM(t, keyFile, "EC PRIVATE KEY", clientKeyDER)

	return caFile, certFile, keyFile
}

func writePEM(t *testing.T, path, pemType string, der []byte) {
	t.Helper()
	f, err := os.Create(path)
	require.NoError(t, err)
	defer f.Close()
	require.NoError(t, pem.Encode(f, &pem.Block{Type: pemType, Bytes: der}))
}

func TestConfig_IsSet(t *testing.T) {
	testCases := map[string]struct {
		config Config
		want   bool
	}{
		"all fields set": {
			config: Config{CACert: "ca.crt", TLSCert: "tls.crt", TLSKey: "tls.key"},
			want:   true,
		},
		"empty config": {
			config: Config{},
			want:   false,
		},
		"only ca-cert": {
			config: Config{CACert: "ca.crt"},
			want:   false,
		},
		"only tls-cert and tls-key": {
			config: Config{TLSCert: "tls.crt", TLSKey: "tls.key"},
			want:   false,
		},
		"missing tls-key": {
			config: Config{CACert: "ca.crt", TLSCert: "tls.crt"},
			want:   false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, tc.want, tc.config.IsSet())
		})
	}
}

func TestConfig_Validate(t *testing.T) {
	testCases := map[string]struct {
		config  Config
		wantErr bool
	}{
		"empty config (no certs)": {
			config:  Config{},
			wantErr: false,
		},
		"all fields set": {
			config:  Config{CACert: "ca.crt", TLSCert: "tls.crt", TLSKey: "tls.key"},
			wantErr: false,
		},
		"only ca-cert": {
			config:  Config{CACert: "ca.crt"},
			wantErr: true,
		},
		"only tls-cert": {
			config:  Config{TLSCert: "tls.crt"},
			wantErr: true,
		},
		"only tls-key": {
			config:  Config{TLSKey: "tls.key"},
			wantErr: true,
		},
		"missing tls-key": {
			config:  Config{CACert: "ca.crt", TLSCert: "tls.crt"},
			wantErr: true,
		},
		"missing tls-cert": {
			config:  Config{CACert: "ca.crt", TLSKey: "tls.key"},
			wantErr: true,
		},
		"missing ca-cert": {
			config:  Config{TLSCert: "tls.crt", TLSKey: "tls.key"},
			wantErr: true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			err := tc.config.Validate()
			if tc.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestConfig_TLSConfig(t *testing.T) {
	t.Run("valid certs", func(t *testing.T) {
		caFile, certFile, keyFile := generateTestCerts(t)
		tlsConfig, err := Config{CACert: caFile, TLSCert: certFile, TLSKey: keyFile}.TLSConfig("")
		require.NoError(t, err)
		assert.NotNil(t, tlsConfig.RootCAs)
		assert.NotNil(t, tlsConfig.GetClientCertificate)
		assert.Equal(t, tls.NoClientCert, tlsConfig.ClientAuth) // client config: no ClientAuth
		assert.Empty(t, tlsConfig.ServerName)
	})

	t.Run("server name is set", func(t *testing.T) {
		caFile, certFile, keyFile := generateTestCerts(t)
		tlsConfig, err := Config{CACert: caFile, TLSCert: certFile, TLSKey: keyFile}.TLSConfig("temporal.example.com")
		require.NoError(t, err)
		assert.Equal(t, "temporal.example.com", tlsConfig.ServerName)
	})

	t.Run("non-existent ca cert", func(t *testing.T) {
		_, certFile, keyFile := generateTestCerts(t)
		_, err := Config{CACert: "/nonexistent/ca.crt", TLSCert: certFile, TLSKey: keyFile}.TLSConfig("")
		require.Error(t, err)
		assert.ErrorIs(t, err, os.ErrNotExist)
	})

	t.Run("invalid ca cert content", func(t *testing.T) {
		dir := t.TempDir()
		badCA := filepath.Join(dir, "ca.crt")
		require.NoError(t, os.WriteFile(badCA, []byte("not a certificate"), 0600))
		_, certFile, keyFile := generateTestCerts(t)
		_, err := Config{CACert: badCA, TLSCert: certFile, TLSKey: keyFile}.TLSConfig("")
		require.Error(t, err)
	})

	t.Run("non-existent client cert", func(t *testing.T) {
		caFile, _, keyFile := generateTestCerts(t)
		_, err := Config{CACert: caFile, TLSCert: "/nonexistent/tls.crt", TLSKey: keyFile}.TLSConfig("")
		require.Error(t, err)
	})
}

func TestConfig_ServerTLSConfig(t *testing.T) {
	t.Run("valid certs", func(t *testing.T) {
		caFile, certFile, keyFile := generateTestCerts(t)
		tlsConfig, err := Config{CACert: caFile, TLSCert: certFile, TLSKey: keyFile}.ServerTLSConfig()
		require.NoError(t, err)
		assert.NotEmpty(t, tlsConfig.Certificates)
		assert.NotNil(t, tlsConfig.ClientCAs)
		assert.Equal(t, tls.RequireAndVerifyClientCert, tlsConfig.ClientAuth)
		assert.Nil(t, tlsConfig.RootCAs) // server config: no RootCAs
	})

	t.Run("non-existent ca cert", func(t *testing.T) {
		_, certFile, keyFile := generateTestCerts(t)
		_, err := Config{CACert: "/nonexistent/ca.crt", TLSCert: certFile, TLSKey: keyFile}.ServerTLSConfig()
		require.Error(t, err)
		assert.ErrorIs(t, err, os.ErrNotExist)
	})
}

func TestConfig_DynamicTLSConfig(t *testing.T) {
	for _, partial := range []bool{false, true} {
		name := "valid certificates"
		if partial {
			name = "partial configuration"
		}
		t.Run(name, func(t *testing.T) {
			caFile, certFile, keyFile := generateTestCerts(t)
			config := Config{
				CACert:  caFile,
				TLSCert: certFile,
				TLSKey:  keyFile,
			}
			if partial {
				config.TLSKey = ""
			}
			tlsConfig, dynamicConfig, err := config.DynamicTLSConfig("temporal.example.com")
			if partial {
				require.EqualError(t, err, config.Validate().Error())
				assert.Nil(t, tlsConfig)
				assert.Nil(t, dynamicConfig)
				return
			}
			require.NoError(t, err)
			defer dynamicConfig.Close()

			assert.NotNil(t, tlsConfig.RootCAs)
			assert.NotNil(t, tlsConfig.GetClientCertificate)
			assert.Equal(t, "temporal.example.com", tlsConfig.ServerName)
		})
	}
}

func TestConfig_DynamicServerTLSConfig(t *testing.T) {
	for _, partial := range []bool{false, true} {
		name := "valid certificates"
		if partial {
			name = "partial configuration"
		}
		t.Run(name, func(t *testing.T) {
			caFile, certFile, keyFile := generateTestCerts(t)
			config := Config{
				CACert:  caFile,
				TLSCert: certFile,
				TLSKey:  keyFile,
			}
			if partial {
				config.TLSKey = ""
			}
			tlsConfig, dynamicConfig, err := config.DynamicServerTLSConfig()
			if partial {
				require.EqualError(t, err, config.Validate().Error())
				assert.Nil(t, tlsConfig)
				assert.Nil(t, dynamicConfig)
				return
			}
			require.NoError(t, err)
			defer dynamicConfig.Close()

			currentConfig, err := tlsConfig.GetConfigForClient(nil)
			require.NoError(t, err)
			assert.NotEmpty(t, currentConfig.Certificates)
			assert.NotNil(t, currentConfig.ClientCAs)
			assert.Equal(t, tls.RequireAndVerifyClientCert, currentConfig.ClientAuth)
		})
	}
}
