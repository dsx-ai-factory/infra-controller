// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credentials

import (
	"context"
	"errors"
	"fmt"
	"net"
	"sync"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	log "github.com/sirupsen/logrus"
)

const (
	bmcPrefix  = "bmc:"
	nvosPrefix = "nvos:"
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

func (m *InMemoryCredentialManager) bmcKey(mac net.HardwareAddr) string {
	return bmcPrefix + mac.String()
}

func (m *InMemoryCredentialManager) nvosKey(mac net.HardwareAddr) string {
	return nvosPrefix + mac.String()
}

// GetBMC returns the BMC credential for mac or an error if missing/invalid.
func (m *InMemoryCredentialManager) GetBMC(ctx context.Context, mac net.HardwareAddr) (*credential.Credential, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	key := m.bmcKey(mac)
	cred, exists := m.store[key]
	if !exists {
		return nil, errors.New("BMC credential not found")
	}

	if !cred.IsValid() {
		return nil, errors.New("BMC credential not valid")
	}

	return cred, nil
}

// PutBMC stores the BMC credential for mac. If an identical entry exists, this is a no-op.
// If a different entry exists, the new value overwrites (with a warning log).
func (m *InMemoryCredentialManager) PutBMC(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	if cred == nil {
		return fmt.Errorf("BMC credential for %s is nil", mac)
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	key := m.bmcKey(mac)
	if existing, exists := m.store[key]; exists {
		if existing.Equal(cred) {
			log.Infof("BMC credentials for %s already exist and match; skipping write", mac)
			return nil
		}
		log.Warnf("BMC credentials for %s differ from existing; overwriting in-memory entry", mac)
	}
	m.store[key] = cred
	return nil
}

// PatchBMC updates the BMC credential for mac (replaces current value).
func (m *InMemoryCredentialManager) PatchBMC(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	if cred == nil {
		return fmt.Errorf("BMC credential for %s is nil", mac)
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	key := m.bmcKey(mac)

	if _, exists := m.store[key]; !exists {
		return errors.New("BMC credential not found")
	}

	m.store[key] = cred
	return nil
}

// DeleteBMC removes the BMC credential for mac (no error if absent).
func (m *InMemoryCredentialManager) DeleteBMC(ctx context.Context, mac net.HardwareAddr) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	key := m.bmcKey(mac)

	delete(m.store, key)
	return nil
}

// GetNVOS returns the NVOS credential for mac or an error if missing/invalid.
func (m *InMemoryCredentialManager) GetNVOS(ctx context.Context, mac net.HardwareAddr) (*credential.Credential, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	key := m.nvosKey(mac)
	cred, exists := m.store[key]
	if !exists {
		return nil, errors.New("NVOS credential not found")
	}

	if !cred.IsValid() {
		return nil, errors.New("NVOS credential not valid")
	}

	return cred, nil
}

// PutNVOS stores the NVOS credential for mac. If an identical entry exists, this is a no-op.
// If a different entry exists, the new value overwrites (with a warning log).
func (m *InMemoryCredentialManager) PutNVOS(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	if cred == nil {
		return fmt.Errorf("NVOS credential for %s is nil", mac)
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	key := m.nvosKey(mac)
	if existing, exists := m.store[key]; exists {
		if existing.Equal(cred) {
			log.Infof("NVOS credentials for %s already exist and match; skipping write", mac)
			return nil
		}
		log.Warnf("NVOS credentials for %s differ from existing; overwriting in-memory entry", mac)
	}
	m.store[key] = cred
	return nil
}

// PatchNVOS updates the NVOS credential for mac (replaces current value).
func (m *InMemoryCredentialManager) PatchNVOS(ctx context.Context, mac net.HardwareAddr, cred *credential.Credential) error {
	if cred == nil {
		return fmt.Errorf("NVOS credential for %s is nil", mac)
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	key := m.nvosKey(mac)

	if _, exists := m.store[key]; !exists {
		return errors.New("NVOS credential not found")
	}

	m.store[key] = cred
	return nil
}

// DeleteNVOS removes the NVOS credential for mac (no error if absent).
func (m *InMemoryCredentialManager) DeleteNVOS(ctx context.Context, mac net.HardwareAddr) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	key := m.nvosKey(mac)

	delete(m.store, key)
	return nil
}

// Keys returns all MACs with stored credentials (checking for BMC credentials).
func (m *InMemoryCredentialManager) Keys(ctx context.Context) ([]net.HardwareAddr, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()

	macSet := make(map[string]struct{})
	for key := range m.store {
		// Extract MAC from prefixed key
		var macStr string
		if len(key) > len(bmcPrefix) && key[:len(bmcPrefix)] == bmcPrefix {
			macStr = key[len(bmcPrefix):]
		} else if len(key) > len(nvosPrefix) && key[:len(nvosPrefix)] == nvosPrefix {
			macStr = key[len(nvosPrefix):]
		} else {
			continue
		}
		macSet[macStr] = struct{}{}
	}

	macs := make([]net.HardwareAddr, 0, len(macSet))
	for macStr := range macSet {
		mac, err := net.ParseMAC(macStr)
		if err != nil {
			return nil, err
		}
		macs = append(macs, mac)
	}
	return macs, nil
}
