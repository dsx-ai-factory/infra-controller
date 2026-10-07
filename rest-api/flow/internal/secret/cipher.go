// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package secret encrypts sensitive Flow data before it is persisted or sent
// through Temporal.
package secret

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"
)

// EncryptionKeyPathEnvVar names the environment variable containing the path
// to Flow's mounted data-encryption key.
const EncryptionKeyPathEnvVar = "FLOW_DATA_ENCRYPTION_KEY_PATH"

// EncryptedDataVersion identifies the persisted ciphertext envelope format.
const EncryptedDataVersion uint32 = 1

// encryptedDataEnvelopeDomain is part of the persisted envelope contract.
// Changing it makes existing ciphertext undecryptable.
const encryptedDataEnvelopeDomain = "flow-encrypted-data-envelope"

// EncryptedData is a serializable, versioned ciphertext envelope. KeyID is a
// non-secret SHA-256 fingerprint used to reject key mismatches explicitly and
// to support a future multi-key rotation workflow.
type EncryptedData struct {
	Version    uint32 `json:"version"`
	KeyID      string `json:"key_id"`
	Ciphertext []byte `json:"ciphertext"`
}

// Cipher encrypts and decrypts sensitive data using AES-GCM.
type Cipher struct {
	aead  cipher.AEAD
	keyID string
}

// NewCipher decodes a base64-encoded 256-bit key and constructs a Cipher.
func NewCipher(key string) (*Cipher, error) {
	key = strings.TrimSpace(key)
	if key == "" {
		return nil, errors.New("data encryption key is empty")
	}

	keyBytes, err := base64.StdEncoding.DecodeString(key)
	if err != nil {
		return nil, fmt.Errorf("data encryption key must be base64 encoded: %w", err)
	}
	if len(keyBytes) != 32 {
		return nil, errors.New("data encryption key must decode to exactly 32 bytes")
	}

	block, err := aes.NewCipher(keyBytes)
	if err != nil {
		return nil, fmt.Errorf("create data encryption cipher: %w", err)
	}

	aead, err := cipher.NewGCM(block)
	if err != nil {
		return nil, fmt.Errorf("create data encryption GCM: %w", err)
	}

	keyDigest := sha256.Sum256(keyBytes)

	return &Cipher{
		aead:  aead,
		keyID: hex.EncodeToString(keyDigest[:]),
	}, nil
}

// NewCipherFromFile reads and trims the key at path before constructing a
// Cipher. Kubernetes Secret volume files may include a trailing newline.
func NewCipherFromFile(path string) (*Cipher, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read data encryption key %q: %w", path, err)
	}

	return NewCipher(string(data))
}

// encrypt returns nonce-prefixed AES-GCM ciphertext bound to additionalData.
// Callers must use purpose-specific data and limit each key to substantially
// fewer than 2^32 encryptions to keep random-nonce collision risk negligible.
func (c *Cipher) encrypt(plaintext, additionalData []byte) ([]byte, error) {
	if len(additionalData) == 0 {
		return nil, errors.New("additional authenticated data is empty")
	}

	nonce := make([]byte, c.aead.NonceSize())
	_, err := io.ReadFull(rand.Reader, nonce)
	if err != nil {
		return nil, fmt.Errorf("generate data encryption nonce: %w", err)
	}

	return c.aead.Seal(nonce, nonce, plaintext, additionalData), nil
}

// EncryptData encrypts plaintext into the current persisted envelope format.
// The caller-provided additionalData and envelope metadata are authenticated.
func (c *Cipher) EncryptData(
	plaintext, additionalData []byte,
) (*EncryptedData, error) {
	if len(additionalData) == 0 {
		return nil, errors.New("additional authenticated data is empty")
	}

	version := EncryptedDataVersion
	ciphertext, err := c.encrypt(
		plaintext,
		envelopeAdditionalData(additionalData, version, c.keyID),
	)
	if err != nil {
		return nil, err
	}

	return &EncryptedData{
		Version:    version,
		KeyID:      c.keyID,
		Ciphertext: ciphertext,
	}, nil
}

// decrypt opens nonce-prefixed AES-GCM ciphertext using the same additionalData
// supplied during encryption.
func (c *Cipher) decrypt(ciphertext, additionalData []byte) ([]byte, error) {
	if len(additionalData) == 0 {
		return nil, errors.New("additional authenticated data is empty")
	}

	nonceSize := c.aead.NonceSize()
	if len(ciphertext) < nonceSize {
		return nil, errors.New("encrypted data is too short")
	}

	nonce := ciphertext[:nonceSize]
	plaintext, err := c.aead.Open(nil, nonce, ciphertext[nonceSize:], additionalData)
	if err != nil {
		return nil, fmt.Errorf("decrypt data: %w", err)
	}

	return plaintext, nil
}

// DecryptData validates and decrypts a persisted ciphertext envelope.
func (c *Cipher) DecryptData(
	encrypted *EncryptedData,
	additionalData []byte,
) ([]byte, error) {
	if encrypted == nil {
		return nil, errors.New("encrypted data is nil")
	}
	if len(additionalData) == 0 {
		return nil, errors.New("additional authenticated data is empty")
	}
	if encrypted.Version != EncryptedDataVersion {
		return nil, fmt.Errorf(
			"unsupported encrypted data version %d",
			encrypted.Version,
		)
	}
	if encrypted.KeyID == "" {
		return nil, errors.New("encrypted data key ID is empty")
	}
	if encrypted.KeyID != c.keyID {
		return nil, fmt.Errorf(
			"encrypted data key ID %q does not match configured key ID",
			encrypted.KeyID,
		)
	}

	return c.decrypt(
		encrypted.Ciphertext,
		envelopeAdditionalData(
			additionalData,
			encrypted.Version,
			encrypted.KeyID,
		),
	)
}

func envelopeAdditionalData(
	additionalData []byte,
	version uint32,
	keyID string,
) []byte {
	hash := sha256.New()
	_, _ = hash.Write([]byte(encryptedDataEnvelopeDomain))
	var versionBytes [4]byte
	binary.BigEndian.PutUint32(versionBytes[:], version)
	_, _ = hash.Write(versionBytes[:])
	_, _ = hash.Write([]byte(keyID))
	_, _ = hash.Write(additionalData)
	return hash.Sum(nil)
}
