// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credential

import (
	"os"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/secretstring"
)

// Credential holds authentication information with password protection
type Credential struct {
	User     string                    `json:"user"`     // User name
	Password secretstring.SecretString `json:"password"` // Password (masked in JSON/logs)
}

// New creates a Credential with the given user and password.
func New(user string, password string) Credential {
	return Credential{
		User:     user,
		Password: secretstring.New(password),
	}
}

// NewFromEnv creates a Credential from environment variables as-is
func NewFromEnv(userEnv string, passwordEnv string) Credential {
	return Credential{
		User:     os.Getenv(userEnv),
		Password: secretstring.New(os.Getenv(passwordEnv)),
	}
}

// Patch updates the credential with non-empty values from the given
// credential. It returns true if any field was updated.
func (cred *Credential) Patch(nc *Credential) bool {
	if cred == nil || nc == nil {
		return false
	}

	patched := false

	if strings.TrimSpace(nc.User) != "" && cred.User != nc.User {
		cred.User = nc.User
		patched = true
	}

	if !nc.Password.IsEmpty() && !cred.Password.IsEqual(nc.Password) {
		cred.Password = nc.Password
		patched = true
	}

	return patched
}

// Equal returns true if both credentials have the same username and password.
func (cred *Credential) Equal(other *Credential) bool {
	if cred == nil || other == nil {
		return cred == other
	}
	return cred.User == other.User && cred.Password.IsEqual(other.Password)
}

// IsValid returns true if the credential has a non-empty username
// Note: Password validation is intentionally not included for flexibility
func (cred *Credential) IsValid() bool {
	return strings.TrimSpace(cred.User) != ""
}

// Update modifies credential fields if the provided pointers are not nil
func (cred *Credential) Update(user *string, password *string) {
	if user != nil {
		cred.User = *user
	}

	if password != nil {
		cred.Password.Value = *password
	}
}

// Retrieve returns pointers to user and password if credential is valid
// Returns nil pointers if credential is invalid
func (cred *Credential) Retrieve() (*string, *string) {
	if !cred.IsValid() {
		return nil, nil
	}

	// Create a copy to avoid exposing internal state
	c := *cred
	return &c.User, &c.Password.Value
}
