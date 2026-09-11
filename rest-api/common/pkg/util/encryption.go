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
	// secretLogPrefixLen is how much of a secret a diagnostic log may carry. The
	// shortest secret this covers is a Site registration OTP, 20 random bytes in
	// base64, so four characters tell two of them apart and leave the remaining
	// 136 bits out of the logs.
	secretLogPrefixLen = 4

	// redactedMarker stands in for the withheld remainder. It repeats the text of
	// grpcproxy.RedactedPlaceholder rather than importing it, because a test in
	// that package already imports this one.
	redactedMarker = "[REDACTED]"
)

// RedactSecret renders a secret for a diagnostic log: a short identifying
// prefix, a marker for the withheld remainder, and the length. That is enough
// to tell which value a failure was about, and never enough to reuse it. A
// secret no longer than the prefix keeps none of its characters.
func RedactSecret(secret string) string {
	if len(secret) <= secretLogPrefixLen {
		return fmt.Sprintf("%s len=%d", redactedMarker, len(secret))
	}
	return fmt.Sprintf("%s%s len=%d", secret[:secretLogPrefixLen], redactedMarker, len(secret))
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
