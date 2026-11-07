// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

// PayloadEncryptionConfig holds configuration for payload encryption
type PayloadEncryptionConfig struct {
	EncryptionKey string
}

// NewPayloadEncryptionConfig initializes and returns a configuration object for payload encryption
func NewPayloadEncryptionConfig(encryptionKey string) *PayloadEncryptionConfig {
	return &PayloadEncryptionConfig{
		EncryptionKey: encryptionKey,
	}
}
