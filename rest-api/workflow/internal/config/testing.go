// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"math/big"
	"os"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
)

var (
	testCfg *Config
)

// GetTestConfig returns a config for tests
func GetTestConfig() *Config {
	if testCfg == nil {
		testCfg = NewConfig()
	}
	return testCfg
}

// SetupTestCerts sets up a test key and cert
func SetupTestCerts(t *testing.T) (string, string) {
	keyPath := "/tmp/tls.key"
	certPath := "/tmp/tls.crt"

	// Generate keypair for test
	privatekey, err := rsa.GenerateKey(rand.Reader, 2048)
	assert.NoError(t, err)
	publickey := &privatekey.PublicKey

	// Dump private key to file
	privateKeyBytes := x509.MarshalPKCS1PrivateKey(privatekey)
	privateKeyBlock := &pem.Block{
		Type:  "RSA PRIVATE KEY",
		Bytes: privateKeyBytes,
	}

	privatePem, err := os.Create(keyPath)
	assert.NoError(t, err)

	err = pem.Encode(privatePem, privateKeyBlock)
	assert.NoError(t, err)

	tml := x509.Certificate{
		// you can add any attr that you need
		NotBefore: time.Now(),
		NotAfter:  time.Now().AddDate(5, 0, 0),
		// you have to generate a different serial number each execution
		SerialNumber: big.NewInt(123123),
		Subject: pkix.Name{
			CommonName:   "New Name",
			Organization: []string{"New Org."},
		},
		BasicConstraintsValid: true,
	}
	cert, err := x509.CreateCertificate(rand.Reader, &tml, &tml, publickey, privatekey)
	assert.NoError(t, err)

	// Generate a pem block with the certificate
	certBlock := &pem.Block{
		Type:  "CERTIFICATE",
		Bytes: cert,
	}

	certPem, err := os.Create(certPath)
	assert.NoError(t, err)

	err = pem.Encode(certPem, certBlock)
	assert.NoError(t, err)

	return keyPath, certPath
}
