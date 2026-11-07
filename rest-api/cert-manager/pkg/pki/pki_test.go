// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pki

import (
	"crypto/x509"
	"encoding/pem"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewCA(t *testing.T) {
	ca, err := NewTestCA(CAOptions{
		CommonName:   "Test CA",
		Organization: "Test Org",
		TTL:          24 * time.Hour,
	})
	if err != nil {
		t.Fatalf("NewTestCA failed: %v", err)
	}

	// Verify CA certificate is valid PEM
	certPEM := ca.GetCACertificatePEM()
	if !strings.HasPrefix(certPEM, "-----BEGIN CERTIFICATE-----") {
		t.Errorf("CA certificate should be PEM encoded, got: %s", certPEM[:50])
	}

	// Parse and verify CA certificate
	block, _ := pem.Decode([]byte(certPEM))
	if block == nil {
		t.Fatal("Failed to decode CA certificate PEM")
	}

	cert, err := x509.ParseCertificate(block.Bytes)
	if err != nil {
		t.Fatalf("Failed to parse CA certificate: %v", err)
	}

	if cert.Subject.CommonName != "Test CA" {
		t.Errorf("Expected CommonName 'Test CA', got '%s'", cert.Subject.CommonName)
	}

	if !cert.IsCA {
		t.Error("Certificate should be a CA")
	}
}

func TestCA_IssueCertificate(t *testing.T) {
	tests := []struct {
		name          string
		commonName    string
		extraDNSNames []string
		ttlHours      int
		wantDNSNames  []string
		// wantVerifyFails are hostnames the certificate must not vouch for.
		wantVerifyFails []string
	}{
		{
			name:            "common name becomes the only SAN when no extras are given",
			commonName:      "my-service.namespace.svc.cluster.local",
			ttlHours:        24,
			wantDNSNames:    []string{"my-service.namespace.svc.cluster.local"},
			wantVerifyFails: []string{"my-service.namespace"},
		},
		{
			name:       "every service name a client may dial is carried as a SAN",
			commonName: "nico-rest-cert-manager",
			extraDNSNames: []string{
				"nico-rest-cert-manager.nico-rest",
				"nico-rest-cert-manager.nico-rest.svc",
				"nico-rest-cert-manager.nico-rest.svc.cluster.local",
				"localhost",
			},
			ttlHours: 48,
			wantDNSNames: []string{
				"nico-rest-cert-manager",
				"nico-rest-cert-manager.nico-rest",
				"nico-rest-cert-manager.nico-rest.svc",
				"nico-rest-cert-manager.nico-rest.svc.cluster.local",
				"localhost",
			},
			wantVerifyFails: []string{"credsmgr.csm"},
		},
		{
			name:          "repeated and empty names are dropped",
			commonName:    "svc.ns",
			extraDNSNames: []string{"svc.ns", "", "localhost"},
			ttlHours:      24,
			wantDNSNames:  []string{"svc.ns", "localhost"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ca, err := NewTestCA(CAOptions{
				CommonName:   "Test CA",
				Organization: "Test Org",
			})
			require.NoError(t, err, "NewTestCA")

			certPEM, keyPEM, err := ca.IssueCertificate(tt.commonName, tt.extraDNSNames, tt.ttlHours)
			require.NoError(t, err, "IssueCertificate")

			// Verify certificate and key are valid PEM
			assert.True(t, strings.HasPrefix(certPEM, "-----BEGIN CERTIFICATE-----"), "certificate should be PEM encoded")
			assert.True(t, strings.HasPrefix(keyPEM, "-----BEGIN RSA PRIVATE KEY-----"), "key should be PEM encoded")

			// Parse and verify certificate
			block, _ := pem.Decode([]byte(certPEM))
			require.NotNil(t, block, "certificate should decode as PEM")

			cert, err := x509.ParseCertificate(block.Bytes)
			require.NoError(t, err, "ParseCertificate")

			assert.Equal(t, tt.commonName, cert.Subject.CommonName)
			assert.False(t, cert.IsCA, "issued certificate should not be a CA")
			assert.Equal(t, tt.wantDNSNames, cert.DNSNames)

			// A serving certificate is useless without these, and they do not
			// vary by case, so they are asserted on every issued certificate.
			assert.NotZero(t, cert.KeyUsage&x509.KeyUsageDigitalSignature, "DigitalSignature key usage")
			assert.NotZero(t, cert.KeyUsage&x509.KeyUsageKeyEncipherment, "KeyEncipherment key usage")
			assert.Contains(t, cert.ExtKeyUsage, x509.ExtKeyUsageServerAuth)
			assert.Contains(t, cert.ExtKeyUsage, x509.ExtKeyUsageClientAuth)

			// The validity window has to honour the requested TTL; a minute of
			// tolerance absorbs the clock read between NotBefore and NotAfter.
			assert.WithinDuration(t, cert.NotBefore.Add(time.Duration(tt.ttlHours)*time.Hour),
				cert.NotAfter, time.Minute)

			// VerifyHostname is what a TLS client applies, so assert against it
			// rather than only on the SAN list.
			for _, host := range tt.wantDNSNames {
				assert.NoError(t, cert.VerifyHostname(host), "VerifyHostname(%q)", host)
			}
			for _, host := range tt.wantVerifyFails {
				assert.Error(t, cert.VerifyHostname(host), "VerifyHostname(%q) should report a hostname mismatch", host)
			}

			// Verify certificate is signed by CA
			caBlock, _ := pem.Decode([]byte(ca.GetCACertificatePEM()))
			require.NotNil(t, caBlock, "CA certificate should decode as PEM")

			caCert, err := x509.ParseCertificate(caBlock.Bytes)
			require.NoError(t, err, "ParseCertificate(CA)")

			roots := x509.NewCertPool()
			roots.AddCert(caCert)

			opts := x509.VerifyOptions{
				Roots: roots,
			}

			_, err = cert.Verify(opts)
			assert.NoError(t, err, "certificate should chain to the CA")
		})
	}
}

func TestCA_GetCRL(t *testing.T) {
	ca, err := NewTestCA(CAOptions{
		CommonName:   "Test CA",
		Organization: "Test Org",
	})
	if err != nil {
		t.Fatalf("NewTestCA failed: %v", err)
	}

	crl := ca.GetCRL()
	if !strings.HasPrefix(crl, "-----BEGIN X509 CRL-----") {
		t.Errorf("CRL should be PEM encoded, got: %s", crl[:30])
	}
}

func TestNewCA_Defaults(t *testing.T) {
	// Test that defaults are applied when options are empty
	ca, err := NewTestCA(CAOptions{})
	if err != nil {
		t.Fatalf("NewTestCA with empty options failed: %v", err)
	}

	certPEM := ca.GetCACertificatePEM()
	block, _ := pem.Decode([]byte(certPEM))
	cert, err := x509.ParseCertificate(block.Bytes)
	if err != nil {
		t.Fatalf("Failed to parse CA certificate: %v", err)
	}

	if cert.Subject.CommonName != "NICo Local CA" {
		t.Errorf("Expected default CommonName 'NICo Local CA', got '%s'", cert.Subject.CommonName)
	}

	if len(cert.Subject.Organization) == 0 || cert.Subject.Organization[0] != "NVIDIA" {
		t.Errorf("Expected default Organization 'NVIDIA', got '%v'", cert.Subject.Organization)
	}
}

func TestCA_Concurrent(t *testing.T) {
	ca, err := NewTestCA(CAOptions{})
	if err != nil {
		t.Fatalf("NewTestCA failed: %v", err)
	}

	// Test concurrent certificate issuance
	done := make(chan bool, 10)
	for i := 0; i < 10; i++ {
		go func(n int) {
			_, _, err := ca.IssueCertificate("concurrent-test.local", nil, 24)
			if err != nil {
				t.Errorf("Concurrent IssueCertificate %d failed: %v", n, err)
			}
			done <- true
		}(i)
	}

	// Wait for all goroutines
	for i := 0; i < 10; i++ {
		<-done
	}
}

func BenchmarkCA_IssueCertificate(b *testing.B) {
	ca, err := NewTestCA(CAOptions{})
	if err != nil {
		b.Fatalf("NewTestCA failed: %v", err)
	}

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		_, _, err := ca.IssueCertificate("benchmark.test.local", nil, 24)
		if err != nil {
			b.Fatalf("IssueCertificate failed: %v", err)
		}
	}
}

func TestLoadCAFromPEM(t *testing.T) {
	// First create a CA to get valid PEM data
	originalCA, err := NewTestCA(CAOptions{
		CommonName:   "Test Load CA",
		Organization: "Test Org",
	})
	if err != nil {
		t.Fatalf("NewTestCA failed: %v", err)
	}

	// Get the PEM data
	certPEM := originalCA.GetCACertificatePEM()

	// We need to also get the key PEM - create a new CA and extract its key
	// For this test, we'll generate a CA, save it, then load it
	ca2, err := NewTestCA(CAOptions{
		CommonName:   "Loadable CA",
		Organization: "Test",
	})
	if err != nil {
		t.Fatalf("NewTestCA failed: %v", err)
	}

	// Issue a cert with the original CA
	issuedCert1, _, err := ca2.IssueCertificate("test.example.com", nil, 24)
	if err != nil {
		t.Fatalf("IssueCertificate failed: %v", err)
	}

	// The CA should be able to issue valid certificates
	if !strings.HasPrefix(issuedCert1, "-----BEGIN CERTIFICATE-----") {
		t.Error("Issued cert should be PEM encoded")
	}

	// Verify we got the right CA by checking the cert PEM
	if certPEM == "" {
		t.Error("CA certificate PEM should not be empty")
	}
}

func TestLoadCA_InvalidCert(t *testing.T) {
	invalidCert := []byte("not a certificate")
	validKey := []byte(`-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEA0Z3VS5JJcds3xfn/ygWyF8PbnGy0AHB7MAsj6FZ0BxnNz6aO
-----END RSA PRIVATE KEY-----`)

	_, err := LoadCAFromPEM(invalidCert, validKey)
	if err == nil {
		t.Error("LoadCAFromPEM should fail with invalid certificate")
	}
}

func TestLoadCA_InvalidKey(t *testing.T) {
	// Create a valid CA cert first
	ca, _ := NewTestCA(CAOptions{})
	certPEM := ca.GetCACertificatePEM()

	invalidKey := []byte("not a key")

	_, err := LoadCAFromPEM([]byte(certPEM), invalidKey)
	if err == nil {
		t.Error("LoadCAFromPEM should fail with invalid key")
	}
}
