// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bootstraptypes

import (
	"context"
	"errors"
	"fmt"
	"sync"

	"go.uber.org/atomic"
	coreV1Types "k8s.io/client-go/kubernetes/typed/core/v1"
)

// Tags must match those in SecretConfig
const (
	TagUUID     = "site-uuid"
	TagOTP      = "otp"
	TagCredsURL = "creds-url"
	TagCACert   = "cacert"
)

// SecretConfig Secret file contents of the data section
type SecretConfig struct {
	UUID     string `yaml:"site-uuid"`
	OTP      string `yaml:"otp"`
	CredsURL string `yaml:"creds-url"`
	CACert   string `yaml:"cacert"`
}

// SecretReq Secret request data body
type SecretReq struct {
	UUID string `json:"siteuuid"`
	OTP  string `json:"otp"`
}

// SiteCredsResponse defines a site credentials response
type SiteCredsResponse struct {
	// Key is the private key
	Key string `json:"key,omitempty"`
	// Certificate is the client certificate
	Certificate string `json:"certificate,omitempty"`
	// CACertificate is the CA cert for validating the server
	CACertificate string `json:"cacertificate,omitempty"`
}

// State - state of the bootstrap process
type State struct {
	// DownloadSucceeded the Credentials file has been updated
	DownloadSucceeded atomic.Uint64
	// DownloadAttempted the number of times the secret file has been updated
	DownloadAttempted atomic.Uint64
}

// Registration is what the Site Agent applied from its site-registration Secret: the
// Site ID and OTP it read at startup, plus each OTP it wrote back through OTP rotation.
// Any other Site ID or OTP in that Secret comes from a re-pair.
type Registration struct {
	mu     sync.Mutex
	siteID string
	otps   map[string]bool
}

// Load records the Site ID and OTP the Site Agent read at startup.
func (r *Registration) Load(siteID, otp string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.siteID = siteID
	r.otps = map[string]bool{otp: true}
}

// AddOTP records an OTP the Site Agent writes to site-registration through OTP rotation.
func (r *Registration) AddOTP(otp string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.otps == nil {
		r.otps = make(map[string]bool)
	}
	r.otps[otp] = true
}

// Check returns an error when siteID or otp, read back from site-registration, is not one
// the Site Agent applied. It returns nil before Load, as on a pod that does not run the
// bootstrap.
func (r *Registration) Check(siteID, otp string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.siteID == "" {
		return nil
	}
	if siteID != r.siteID {
		return fmt.Errorf("site-registration holds Site ID %s, not %s", siteID, r.siteID)
	}
	if !r.otps[otp] {
		return errors.New("site-registration holds an OTP the Site Agent did not apply")
	}
	return nil
}

// Bootstrap - data type for Bootstrap
type Bootstrap struct {
	State        *State
	Secret       coreV1Types.SecretInterface
	Secretfiles  map[string]bool
	Registration *Registration
	config       SecretConfig
	mutex        sync.RWMutex // protects config
	updateMutex  sync.Mutex   // serializes credential download and publication
}

// GetConfig returns a consistent copy of the active bootstrap configuration.
func (b *Bootstrap) GetConfig() SecretConfig {
	b.mutex.RLock()
	defer b.mutex.RUnlock()
	return b.config
}

// ReplaceConfig atomically replaces all bootstrap configuration fields.
func (b *Bootstrap) ReplaceConfig(config SecretConfig) {
	b.mutex.Lock()
	defer b.mutex.Unlock()
	b.config = config
}

// RunCredentialUpdate serializes one credential download, store, and publication operation.
// The callback receives a consistent copy of the active configuration; its returned
// configuration is published only after the operation succeeds.
func (b *Bootstrap) RunCredentialUpdate(ctx context.Context, update func(SecretConfig) (SecretConfig, error)) error {
	b.updateMutex.Lock()
	defer b.updateMutex.Unlock()

	err := ctx.Err()
	if err != nil {
		return err
	}
	config := b.GetConfig()
	updatedConfig, err := update(config)
	if err != nil {
		return err
	}
	b.ReplaceConfig(updatedConfig)
	return nil
}

// NewBootstrapInstance - creates a new instance of Bootstrap
func NewBootstrapInstance() *Bootstrap {
	return &Bootstrap{
		State:        &State{},
		Secretfiles:  make(map[string]bool),
		Registration: &Registration{},
	}
}
