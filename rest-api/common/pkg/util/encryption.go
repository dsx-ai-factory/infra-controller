// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"crypto/sha256"
	"fmt"
	"io"

	"github.com/rs/zerolog/log"
)

const (
	// SecretLogPrefixLen is how much of an opaque secret a diagnostic log may
	// carry. The shortest one this covers is a Site registration OTP, 20 random
	// bytes in base64, so four characters tell two of them apart and leave the
	// remaining 136 bits out of the logs.
	SecretLogPrefixLen = 4

	// CertLogPrefixLen is how much of a PEM a diagnostic log may carry. Its
	// `-----BEGIN CERTIFICATE-----` line takes the first 28 characters, so a
	// prefix has to clear that before it reaches anything that tells two
	// certificates apart. This leaves 36 characters of the encoded body, and a
	// certificate is public in any case.
	CertLogPrefixLen = 64

	// redactedMarker stands in for the withheld remainder. It repeats the text of
	// grpcproxy.RedactedPlaceholder rather than importing it, because a test in
	// that package already imports this one.
	redactedMarker = "[REDACTED]"
)

// RedactSecret renders a secret for a diagnostic log: up to prefixLen leading
// characters, a marker for the withheld remainder, and the length. That is
// enough to tell which value a failure was about, and never enough to reuse it.
// Pass SecretLogPrefixLen for an opaque secret or CertLogPrefixLen for a PEM. A
// secret no longer than prefixLen keeps none of its characters, so asking for
// more than the value holds yields nothing rather than all of it.
func RedactSecret(secret string, prefixLen int) string {
	if prefixLen < 0 || len(secret) <= prefixLen {
		return fmt.Sprintf("%s len=%d", redactedMarker, len(secret))
	}
	return fmt.Sprintf("%s%s len=%d", secret[:prefixLen], redactedMarker, len(secret))
}

// CreateHash takes a string and returns SHA 256 digest in a byte array
func CreateHash(key string) []byte {
	hasher := sha256.New()
	_, err := hasher.Write([]byte(key))
	if err != nil {
		log.Panic().Err(err).Msg("error calculating hash for data en/decryption")
	}

	return hasher.Sum(nil)
}

// EncryptData provides mechanism to encrypt arguments being passed into workflows
// so it is not visible within Temporal system
func EncryptData(data []byte, passphrase string) []byte {
	key := CreateHash(passphrase)
	block, err := aes.NewCipher(key)
	if err != nil {
		log.Panic().Err(err).Msg("failed to decrypt data, could not create cipher block")
	}
	gcm, err := cipher.NewGCM(block)
	if err != nil {
		log.Panic().Err(err).Msg("failed to encrypt data, could not create GCM wrapped cipher block")
	}
	nonce := make([]byte, gcm.NonceSize())
	if _, err = io.ReadFull(rand.Reader, nonce); err != nil {
		log.Panic().Err(err).Msg("failed to encrypt data, could not create nonce")
	}
	ciphertext := gcm.Seal(nonce, nonce, data, nil)
	return ciphertext
}

// DecryptData provides mechanism to decrypt arguments being passed into workflows
func DecryptData(data []byte, passphrase string) []byte {
	key := CreateHash(passphrase)
	block, err := aes.NewCipher(key)
	if err != nil {
		log.Panic().Err(err).Msg("failed to decrypt data, could not create cipher block")
	}
	gcm, err := cipher.NewGCM(block)
	if err != nil {
		log.Panic().Err(err).Msg("failed to decrypt data, could not create GCM wrapped cipher block")
	}
	nonceSize := gcm.NonceSize()
	nonce, ciphertext := data[:nonceSize], data[nonceSize:]
	plaintext, err := gcm.Open(nil, nonce, ciphertext, nil)
	if err != nil {
		panic(err.Error())
	}
	return plaintext
}
