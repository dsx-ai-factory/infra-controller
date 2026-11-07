// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"bytes"
	"crypto/sha256"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestCreateHash(t *testing.T) {
	key := "key"
	expectedHash := sha256.Sum256([]byte(key))

	hash := CreateHash(key)

	if !bytes.Equal(hash, expectedHash[:]) {
		t.Errorf("Expected hash %x but got %x", expectedHash, hash)
	}
}

func TestEncryptAndDecryptData(t *testing.T) {
	data := []byte("this is some data to encrypt")
	passphrase := "testpassphrase"

	// Encrypt the data
	encryptedData := EncryptData(data, passphrase)

	if len(encryptedData) == 0 {
		t.Fatal("Expected encrypted data, but got empty byte slice")
	}

	// Decrypt the data
	decryptedData := DecryptData(encryptedData, passphrase)

	// Verify that decrypted data matches the original data
	if !bytes.Equal(decryptedData, data) {
		t.Errorf("Expected decrypted data to be %s but got %s", data, decryptedData)
	}
}

func TestEncryptAndDecryptWithWrongPassphrase(t *testing.T) {
	data := []byte("this is some data to encrypt")
	passphrase := "testpassphrase"
	wrongPassphrase := "wrongpassphrase"

	// Encrypt the data
	encryptedData := EncryptData(data, passphrase)

	defer func() {
		if r := recover(); r == nil {
			t.Fatal("Expected panic but did not get one")
		}
	}()

	// Attempt to decrypt with wrong passphrase (should cause a panic)
	DecryptData(encryptedData, wrongPassphrase)
}

func TestRedactSecret(t *testing.T) {
	// pemBody stands in for the base64 of a certificate, where the prefix has to
	// reach past the BEGIN line to carry anything identifying.
	pemBody := strings.Repeat("A", 100)

	tcs := []struct {
		descr     string
		secret    string
		prefixLen int
		want      string
	}{
		{
			descr:     "registration OTP keeps an identifying prefix",
			secret:    "8Nn5Qk0mVQqHqk2hXwfXQz1Yk5A=",
			prefixLen: SecretLogPrefixLen,
			want:      "8Nn5[REDACTED] len=28",
		},
		{
			descr:     "certificate keeps its BEGIN line and part of the body",
			secret:    "-----BEGIN CERTIFICATE-----\n" + pemBody,
			prefixLen: CertLogPrefixLen,
			want:      "-----BEGIN CERTIFICATE-----\n" + strings.Repeat("A", 36) + "[REDACTED] len=128",
		},
		{
			descr:     "value no longer than the prefix keeps nothing",
			secret:    "8Nn5",
			prefixLen: SecretLogPrefixLen,
			want:      "[REDACTED] len=4",
		},
		{
			descr:     "certificate prefix on a short value keeps nothing",
			secret:    "8Nn5Qk0mVQqHqk2hXwfXQz1Yk5A=",
			prefixLen: CertLogPrefixLen,
			want:      "[REDACTED] len=28",
		},
		{
			descr:     "negative prefix keeps nothing",
			secret:    "8Nn5Qk0mVQqHqk2hXwfXQz1Yk5A=",
			prefixLen: -1,
			want:      "[REDACTED] len=28",
		},
		{
			descr:     "absent secret reports its length",
			secret:    "",
			prefixLen: SecretLogPrefixLen,
			want:      "[REDACTED] len=0",
		},
	}
	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			assert.Equal(t, tc.want, RedactSecret(tc.secret, tc.prefixLen))
		})
	}
}
