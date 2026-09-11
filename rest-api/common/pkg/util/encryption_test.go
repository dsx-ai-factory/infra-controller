// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"bytes"
	"crypto/sha256"
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
	tcs := []struct {
		descr  string
		secret string
		want   string
	}{
		{
			descr:  "registration OTP keeps an identifying prefix",
			secret: "8Nn5Qk0mVQqHqk2hXwfXQz1Yk5A=",
			want:   "8Nn5[REDACTED] len=28",
		},
		{
			descr:  "value no longer than the prefix keeps nothing",
			secret: "8Nn5",
			want:   "[REDACTED] len=4",
		},
		{
			descr:  "absent secret reports its length",
			secret: "",
			want:   "[REDACTED] len=0",
		},
	}
	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			assert.Equal(t, tc.want, RedactSecret(tc.secret))
		})
	}
}
