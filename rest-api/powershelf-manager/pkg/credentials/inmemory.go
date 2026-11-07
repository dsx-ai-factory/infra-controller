// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credentials

import (
	"context"
	"errors"
	"fmt"
	"net"
	"sync"

	log "github.com/sirupsen/logrus"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
)

// InMemoryCredentialManager implements the CredentialManager interface with an in-memory store.
type InMemoryCredentialManager struct {
	store map[string]*credential.Credential
	mu    sync.RWMutex
}

func NewInMemoryCredentialManager() *InMemoryCredentialManager {
	return &InMemoryCredentialManager{
		store: make(map[string]*credential.Credential),
	}
}

// Start InMemoryCredentialManager (NO-OP)
func (m *InMemoryCredentialManager) Start(ctx context.Context) error {
	log.Printf("Starting InMem credential manager")
	// No initialization needed for in-memory store
	return nil
}

// Stop InMemoryCredentialManager (NO-OP)
func (m *InMemoryCredentialManager) Stop(ctx context.Context) error {
	log.Printf("Stopping InMem credential manager")
	// No cleanup needed for in-memory store
	return nil
}

// Get returns the credential for mac or an error if missing/invalid.
func (m *InMemoryCredentialManager) Get(ctx context.Context, mac net.HardwareAddr) (*credential.Credential, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	key := mac.String()
	cred, exists := m.store[key]
	if !exists {
		return nil, errors.New("credential not found")
	}

	if !cred.IsValid() {
		return nil, errors.New("credential not found")
	}

	return cred, nil
}

// Put stores the credential for mac. If an identical entry exists, this is a no-op.
// If a different entry exists, the new value overwrites (with a warning log).
func (m *InMemoryCredentialManager) Put(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	if cred == nil {
		return fmt.Errorf("credential for %s is nil", mac)
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	key := mac.String()
	if existing, exists := m.store[key]; exists {
		if existing.Equal(cred) {
			log.Infof("PMC credentials for %s already exist and match; skipping write", mac)
			return nil
		}
		log.Warnf("PMC credentials for %s differ from existing; overwriting in-memory entry", mac)
	}
	m.store[key] = cred
	return nil
}

// Patch updates the credential for mac (replaces current value).
func (m *InMemoryCredentialManager) Patch(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	if cred == nil {
		return fmt.Errorf("credential for %s is nil", mac)
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	key := mac.String()

	if _, exists := m.store[key]; !exists {
		return errors.New("credential not found")
	}

	// TODO: would it be better to just update the value?
	m.store[key] = cred
	return nil
}

// Delete removes the credential for mac (no error if absent).
func (m *InMemoryCredentialManager) Delete(ctx context.Context, mac net.HardwareAddr) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	key := mac.String()

	delete(m.store, key)
	return nil
}

// Keys returns all MACs with stored credentials.
func (m *InMemoryCredentialManager) Keys(ctx context.Context) ([]net.HardwareAddr, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()

	macs := make([]net.HardwareAddr, 0, len(m.store))
	for key := range m.store {
		mac, err := net.ParseMAC(key)
		if err != nil {
			return nil, err
		}

		if mac != nil {
			macs = append(macs, mac)
		}
	}
	return macs, nil
}
