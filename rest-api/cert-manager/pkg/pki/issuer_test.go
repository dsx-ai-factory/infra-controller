// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pki

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/dsx-ai-factory/infra-controller/rest-api/cert-manager/pkg/types"
)

// createTestCA creates a temporary CA for testing and returns paths to cert and key files
func createTestCA(t *testing.T) (certPath, keyPath string, cleanup func()) {
	t.Helper()

	// Create a CA using the NewTestCA function
	ca, err := NewTestCA(CAOptions{
		CommonName:   "Test CA",
		Organization: "Test Org",
	})
	if err != nil {
		t.Fatalf("Failed to create test CA: %v", err)
	}

	// Write to temp files
	tmpDir := t.TempDir()
	certPath = filepath.Join(tmpDir, "ca.crt")
	keyPath = filepath.Join(tmpDir, "ca.key")

	if err := os.WriteFile(certPath, []byte(ca.GetCACertificatePEM()), 0600); err != nil {
		t.Fatalf("Failed to write CA cert: %v", err)
	}
	if err := os.WriteFile(keyPath, []byte(ca.GetCAKeyPEM()), 0600); err != nil {
		t.Fatalf("Failed to write CA key: %v", err)
	}

	cleanup = func() {
		os.RemoveAll(tmpDir)
	}
	return certPath, keyPath, cleanup
}

func TestNativeCertificateIssuer_NewCertificate(t *testing.T) {
	certPath, keyPath, cleanup := createTestCA(t)
	defer cleanup()

	issuer, err := NewNativeCertificateIssuer(NativeCertificateIssuerOptions{
		BaseDNS:        "test.local",
		CACommonName:   "Test CA",
		CAOrganization: "Test Org",
		CACertFile:     certPath,
		CAKeyFile:      keyPath,
	})
	if err != nil {
		t.Fatalf("NewNativeCertificateIssuer failed: %v", err)
	}

	ctx := context.Background()
	req := &types.CertificateRequest{
		Name: "myservice",
		App:  "myapp",
		TTL:  24,
	}

	cert, key, err := issuer.NewCertificate(ctx, req)
	if err != nil {
		t.Fatalf("NewCertificate failed: %v", err)
	}

	if !strings.HasPrefix(cert, "-----BEGIN CERTIFICATE-----") {
		t.Error("Certificate should be PEM encoded")
	}

	if !strings.HasPrefix(key, "-----BEGIN RSA PRIVATE KEY-----") {
		t.Error("Key should be PEM encoded")
	}
}

func TestNativeCertificateIssuer_RawCertificate(t *testing.T) {
	certPath, keyPath, cleanup := createTestCA(t)
	defer cleanup()

	issuer, err := NewNativeCertificateIssuer(NativeCertificateIssuerOptions{
		BaseDNS:        "test.local",
		CACommonName:   "Test CA",
		CAOrganization: "Test Org",
		CACertFile:     certPath,
		CAKeyFile:      keyPath,
	})
	if err != nil {
		t.Fatalf("NewNativeCertificateIssuer failed: %v", err)
	}

	ctx := context.Background()
	cert, key, err := issuer.RawCertificate(ctx, "raw.test.local", 48)
	if err != nil {
		t.Fatalf("RawCertificate failed: %v", err)
	}

	if !strings.HasPrefix(cert, "-----BEGIN CERTIFICATE-----") {
		t.Error("Certificate should be PEM encoded")
	}

	if !strings.HasPrefix(key, "-----BEGIN RSA PRIVATE KEY-----") {
		t.Error("Key should be PEM encoded")
	}
}

func TestNativeCertificateIssuer_GetCACertificate(t *testing.T) {
	certPath, keyPath, cleanup := createTestCA(t)
	defer cleanup()

	issuer, err := NewNativeCertificateIssuer(NativeCertificateIssuerOptions{
		BaseDNS:        "test.local",
		CACommonName:   "Test CA",
		CAOrganization: "Test Org",
		CACertFile:     certPath,
		CAKeyFile:      keyPath,
	})
	if err != nil {
		t.Fatalf("NewNativeCertificateIssuer failed: %v", err)
	}

	ctx := context.Background()
	caCert, err := issuer.GetCACertificate(ctx)
	if err != nil {
		t.Fatalf("GetCACertificate failed: %v", err)
	}

	if !strings.HasPrefix(caCert, "-----BEGIN CERTIFICATE-----") {
		t.Error("CA Certificate should be PEM encoded")
	}
}

func TestNativeCertificateIssuer_GetCRL(t *testing.T) {
	certPath, keyPath, cleanup := createTestCA(t)
	defer cleanup()

	issuer, err := NewNativeCertificateIssuer(NativeCertificateIssuerOptions{
		BaseDNS:        "test.local",
		CACommonName:   "Test CA",
		CAOrganization: "Test Org",
		CACertFile:     certPath,
		CAKeyFile:      keyPath,
	})
	if err != nil {
		t.Fatalf("NewNativeCertificateIssuer failed: %v", err)
	}

	ctx := context.Background()
	crl, err := issuer.GetCRL(ctx)
	if err != nil {
		t.Fatalf("GetCRL failed: %v", err)
	}

	if !strings.HasPrefix(crl, "-----BEGIN X509 CRL-----") {
		t.Error("CRL should be PEM encoded")
	}
}

func TestNativeCertificateIssuer_NoPathsConfigured(t *testing.T) {
	_, err := NewNativeCertificateIssuer(NativeCertificateIssuerOptions{
		BaseDNS:        "test.local",
		CACommonName:   "Test CA",
		CAOrganization: "Test Org",
	})
	if err == nil {
		t.Fatal("Expected error when no CA paths configured")
	}
	if !strings.Contains(err.Error(), "no paths configured") {
		t.Errorf("Expected 'no paths configured' error, got: %v", err)
	}
}

func TestNativeCertificateIssuer_InvalidPath(t *testing.T) {
	_, err := NewNativeCertificateIssuer(NativeCertificateIssuerOptions{
		BaseDNS:        "test.local",
		CACommonName:   "Test CA",
		CAOrganization: "Test Org",
		CACertFile:     "/nonexistent/ca.crt",
		CAKeyFile:      "/nonexistent/ca.key",
	})
	if err == nil {
		t.Fatal("Expected error when CA files don't exist")
	}
	if !strings.Contains(err.Error(), "not found") {
		t.Errorf("Expected 'not found' error, got: %v", err)
	}
}

func TestNativeCertificateIssuer_AlternatePath(t *testing.T) {
	certPath, keyPath, cleanup := createTestCA(t)
	defer cleanup()

	// Primary paths don't exist, but alternate paths do
	issuer, err := NewNativeCertificateIssuer(NativeCertificateIssuerOptions{
		BaseDNS:        "test.local",
		CACommonName:   "Test CA",
		CAOrganization: "Test Org",
		CACertFile:     "/nonexistent/ca.crt",
		CAKeyFile:      "/nonexistent/ca.key",
		AltCACertFile:  certPath,
		AltCAKeyFile:   keyPath,
	})
	if err != nil {
		t.Fatalf("NewNativeCertificateIssuer with alt paths failed: %v", err)
	}

	ctx := context.Background()
	caCert, err := issuer.GetCACertificate(ctx)
	if err != nil {
		t.Fatalf("GetCACertificate failed: %v", err)
	}

	if !strings.HasPrefix(caCert, "-----BEGIN CERTIFICATE-----") {
		t.Error("CA Certificate should be PEM encoded")
	}
}
