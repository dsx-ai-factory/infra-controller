// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pki

import (
	"crypto/ecdsa"
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"fmt"
	"math/big"
	"time"
)

// CAOptions defines options for creating a test CA
type CAOptions struct {
	CommonName   string
	Organization string
	TTL          time.Duration
}

// NewTestCA creates a new Certificate Authority for testing.
func NewTestCA(opts CAOptions) (*CA, error) {
	if opts.TTL == 0 {
		opts.TTL = DefaultCATTL
	}
	if opts.CommonName == "" {
		opts.CommonName = "NICo Local CA"
	}
	if opts.Organization == "" {
		opts.Organization = "NVIDIA"
	}

	// Generate CA private key
	caKey, err := rsa.GenerateKey(rand.Reader, RSAKeySize)
	if err != nil {
		return nil, fmt.Errorf("failed to generate CA key: %w", err)
	}

	// Create CA certificate template
	serialNumber, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
	if err != nil {
		return nil, fmt.Errorf("failed to generate serial number: %w", err)
	}

	now := time.Now()
	caTemplate := &x509.Certificate{
		SerialNumber: serialNumber,
		Subject: pkix.Name{
			CommonName:   opts.CommonName,
			Organization: []string{opts.Organization},
		},
		NotBefore:             now,
		NotAfter:              now.Add(opts.TTL),
		KeyUsage:              x509.KeyUsageCertSign | x509.KeyUsageCRLSign | x509.KeyUsageDigitalSignature,
		BasicConstraintsValid: true,
		IsCA:                  true,
		MaxPathLen:            1,
	}

	// Self-sign the CA certificate
	caCertDER, err := x509.CreateCertificate(rand.Reader, caTemplate, caTemplate, &caKey.PublicKey, caKey)
	if err != nil {
		return nil, fmt.Errorf("failed to create CA certificate: %w", err)
	}

	caCert, err := x509.ParseCertificate(caCertDER)
	if err != nil {
		return nil, fmt.Errorf("failed to parse CA certificate: %w", err)
	}

	// Encode CA certificate to PEM
	certPEM := pem.EncodeToMemory(&pem.Block{
		Type:  "CERTIFICATE",
		Bytes: caCertDER,
	})

	ca := &CA{
		cert:    caCert,
		key:     caKey,
		certPEM: string(certPEM),
		crl:     &CRL{},
	}

	// Initialize empty CRL
	if err := ca.updateCRL(); err != nil {
		return nil, fmt.Errorf("failed to initialize CRL: %w", err)
	}

	return ca, nil
}

// GetCAKeyPEM returns the CA private key in PEM format (for testing)
func (ca *CA) GetCAKeyPEM() string {
	ca.mu.RLock()
	defer ca.mu.RUnlock()

	var keyPEM []byte
	switch k := ca.key.(type) {
	case *rsa.PrivateKey:
		keyPEM = pem.EncodeToMemory(&pem.Block{
			Type:  "RSA PRIVATE KEY",
			Bytes: x509.MarshalPKCS1PrivateKey(k),
		})
	case *ecdsa.PrivateKey:
		derBytes, err := x509.MarshalECPrivateKey(k)
		if err != nil {
			return ""
		}
		keyPEM = pem.EncodeToMemory(&pem.Block{
			Type:  "EC PRIVATE KEY",
			Bytes: derBytes,
		})
	default:
		return ""
	}
	return string(keyPEM)
}
