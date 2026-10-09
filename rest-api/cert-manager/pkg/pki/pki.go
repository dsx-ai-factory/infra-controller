// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package pki provides certificate authority and PKI operations
package pki

import (
	"crypto"
	"crypto/ecdsa"
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"fmt"
	"math/big"
	"os"
	"sync"
	"time"
)

const (
	// RSAKeySize is the default key size for RSA keys
	RSAKeySize = 2048
	// DefaultCATTL is the default TTL for CA certificates (10 years)
	DefaultCATTL = 10 * 365 * 24 * time.Hour
)

// CA represents a Certificate Authority
type CA struct {
	cert    *x509.Certificate
	key     crypto.Signer
	certPEM string
	mu      sync.RWMutex
	crl     *CRL
}

// CRL represents a Certificate Revocation List
type CRL struct {
	list    *x509.RevocationList
	listPEM string
	mu      sync.RWMutex
}

// GetCACertificatePEM returns the CA certificate in PEM format
func (ca *CA) GetCACertificatePEM() string {
	ca.mu.RLock()
	defer ca.mu.RUnlock()
	return ca.certPEM
}

// GetCRL returns the Certificate Revocation List in PEM format
func (ca *CA) GetCRL() string {
	ca.crl.mu.RLock()
	defer ca.crl.mu.RUnlock()
	return ca.crl.listPEM
}

// IssueCertificate issues a new certificate signed by this CA. The subject
// common name is commonName, and the DNS SANs are commonName followed by
// extraDNSNames with duplicates removed. Hostname verification ignores the
// common name, so a server certificate has to list every name its clients
// dial in extraDNSNames.
func (ca *CA) IssueCertificate(commonName string, extraDNSNames []string, ttlHours int) (certPEM, keyPEM string, err error) {
	ca.mu.RLock()
	defer ca.mu.RUnlock()

	// Checked after deduplication rather than on the arguments, because a
	// caller passing only empty strings would otherwise get a certificate
	// carrying no SAN at all, which no verifying client can accept.
	dnsNames := dedupeDNSNames(commonName, extraDNSNames)
	if len(dnsNames) == 0 {
		return "", "", fmt.Errorf("at least one non-empty DNS name is required to issue a certificate")
	}

	// Generate private key for the certificate
	key, err := rsa.GenerateKey(rand.Reader, RSAKeySize)
	if err != nil {
		return "", "", fmt.Errorf("failed to generate key: %w", err)
	}

	// Generate serial number
	serialNumber, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
	if err != nil {
		return "", "", fmt.Errorf("failed to generate serial number: %w", err)
	}

	now := time.Now()
	ttl := time.Duration(ttlHours) * time.Hour

	// Create certificate template
	template := &x509.Certificate{
		SerialNumber: serialNumber,
		Subject: pkix.Name{
			CommonName: commonName,
		},
		NotBefore:             now,
		NotAfter:              now.Add(ttl),
		KeyUsage:              x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth, x509.ExtKeyUsageClientAuth},
		BasicConstraintsValid: true,
		DNSNames:              dnsNames,
	}

	// Sign the certificate with CA
	certDER, err := x509.CreateCertificate(rand.Reader, template, ca.cert, key.Public(), ca.key)
	if err != nil {
		return "", "", fmt.Errorf("failed to create certificate: %w", err)
	}

	// Encode certificate to PEM
	certPEMBytes := pem.EncodeToMemory(&pem.Block{
		Type:  "CERTIFICATE",
		Bytes: certDER,
	})

	// Encode private key to PEM
	keyPEMBytes := pem.EncodeToMemory(&pem.Block{
		Type:  "RSA PRIVATE KEY",
		Bytes: x509.MarshalPKCS1PrivateKey(key),
	})

	return string(certPEMBytes), string(keyPEMBytes), nil
}

// dedupeDNSNames returns commonName followed by extraDNSNames, dropping empty
// and repeated entries while preserving the caller's order.
func dedupeDNSNames(commonName string, extraDNSNames []string) []string {
	names := make([]string, 0, len(extraDNSNames)+1)
	seen := make(map[string]struct{}, len(extraDNSNames)+1)

	for _, name := range append([]string{commonName}, extraDNSNames...) {
		if name == "" {
			continue
		}
		_, duplicate := seen[name]
		if duplicate {
			continue
		}
		seen[name] = struct{}{}
		names = append(names, name)
	}

	return names
}

// updateCRL updates the Certificate Revocation List
func (ca *CA) updateCRL() error {
	ca.crl.mu.Lock()
	defer ca.crl.mu.Unlock()

	now := time.Now()
	template := &x509.RevocationList{
		Number:     big.NewInt(1),
		ThisUpdate: now,
		NextUpdate: now.Add(24 * time.Hour),
	}

	crlDER, err := x509.CreateRevocationList(rand.Reader, template, ca.cert, ca.key)
	if err != nil {
		return fmt.Errorf("failed to create CRL: %w", err)
	}

	crlPEM := pem.EncodeToMemory(&pem.Block{
		Type:  "X509 CRL",
		Bytes: crlDER,
	})

	ca.crl.listPEM = string(crlPEM)
	return nil
}

// LoadCA loads a Certificate Authority from PEM-encoded certificate and key files.
func LoadCA(certFile, keyFile string) (*CA, error) {
	certPEM, err := os.ReadFile(certFile)
	if err != nil {
		return nil, fmt.Errorf("failed to read CA certificate file: %w", err)
	}

	keyPEM, err := os.ReadFile(keyFile)
	if err != nil {
		return nil, fmt.Errorf("failed to read CA private key file: %w", err)
	}

	return LoadCAFromPEM(certPEM, keyPEM)
}

// LoadCAFromPEM loads a Certificate Authority from PEM-encoded bytes
func LoadCAFromPEM(certPEM, keyPEM []byte) (*CA, error) {
	// Parse certificate
	certBlock, _ := pem.Decode(certPEM)
	if certBlock == nil {
		return nil, fmt.Errorf("failed to decode CA certificate PEM")
	}

	cert, err := x509.ParseCertificate(certBlock.Bytes)
	if err != nil {
		return nil, fmt.Errorf("failed to parse CA certificate: %w", err)
	}

	if !cert.IsCA {
		return nil, fmt.Errorf("certificate is not a CA")
	}

	// Parse private key
	keyBlock, _ := pem.Decode(keyPEM)
	if keyBlock == nil {
		return nil, fmt.Errorf("failed to decode CA private key PEM")
	}

	var key crypto.Signer
	switch keyBlock.Type {
	case "RSA PRIVATE KEY":
		key, err = x509.ParsePKCS1PrivateKey(keyBlock.Bytes)
	case "EC PRIVATE KEY":
		key, err = x509.ParseECPrivateKey(keyBlock.Bytes)
	case "PRIVATE KEY":
		parsedKey, parseErr := x509.ParsePKCS8PrivateKey(keyBlock.Bytes)
		if parseErr != nil {
			return nil, fmt.Errorf("failed to parse PKCS8 private key: %w", parseErr)
		}
		switch k := parsedKey.(type) {
		case *rsa.PrivateKey:
			key = k
		case *ecdsa.PrivateKey:
			key = k
		default:
			return nil, fmt.Errorf("unsupported private key type in PKCS8: %T", parsedKey)
		}
	default:
		return nil, fmt.Errorf("unsupported private key type: %s", keyBlock.Type)
	}
	if err != nil {
		return nil, fmt.Errorf("failed to parse CA private key: %w", err)
	}

	ca := &CA{
		cert:    cert,
		key:     key,
		certPEM: string(certPEM),
		crl:     &CRL{},
	}

	if err := ca.updateCRL(); err != nil {
		// CRL not supported by this CA
		ca.crl.listPEM = ""
	}

	return ca, nil
}
