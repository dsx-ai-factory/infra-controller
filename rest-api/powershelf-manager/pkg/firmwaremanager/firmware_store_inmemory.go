// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"fmt"
	"net"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
)

var _ FirmwareUpdateStore = (*InMemoryStore)(nil)

type fwUpdateKey struct {
	mac       string
	component powershelf.Component
}

// InMemoryStore is an in-memory implementation of FirmwareUpdateStore.
// All data is lost when the process exits.
type InMemoryStore struct {
	mu      sync.RWMutex
	updates map[fwUpdateKey]*FirmwareUpdateRecord
}

func NewInMemoryStore() *InMemoryStore {
	return &InMemoryStore{
		updates: make(map[fwUpdateKey]*FirmwareUpdateRecord),
	}
}

func (s *InMemoryStore) Start(context.Context) error { return nil }
func (s *InMemoryStore) Stop(context.Context) error  { return nil }

func (s *InMemoryStore) CreateOrReplace(_ context.Context, mac net.HardwareAddr, component powershelf.Component, versionFrom, versionTo string) (*FirmwareUpdateRecord, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	now := time.Now()
	key := fwUpdateKey{mac: mac.String(), component: component}
	rec := &FirmwareUpdateRecord{
		PmcMacAddress:      mac,
		Component:          component,
		VersionFrom:        versionFrom,
		VersionTo:          versionTo,
		State:              powershelf.FirmwareStateQueued,
		LastTransitionTime: now,
		UpdatedAt:          now,
	}
	s.updates[key] = rec

	copy := *rec
	return &copy, nil
}

func (s *InMemoryStore) Get(_ context.Context, mac net.HardwareAddr, component powershelf.Component) (*FirmwareUpdateRecord, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	key := fwUpdateKey{mac: mac.String(), component: component}
	rec, ok := s.updates[key]
	if !ok {
		return nil, ErrNotFound
	}

	copy := *rec
	return &copy, nil
}

func (s *InMemoryStore) GetAllPending(_ context.Context) ([]*FirmwareUpdateRecord, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	var results []*FirmwareUpdateRecord
	for _, rec := range s.updates {
		if !rec.IsTerminal() {
			copy := *rec
			results = append(results, &copy)
		}
	}
	return results, nil
}

func (s *InMemoryStore) SetState(_ context.Context, mac net.HardwareAddr, component powershelf.Component, newState powershelf.FirmwareState, errMsg string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	key := fwUpdateKey{mac: mac.String(), component: component}
	rec, ok := s.updates[key]
	if !ok {
		return fmt.Errorf("firmware update not found for %s/%s", mac, component)
	}

	if rec.State == newState && rec.ErrorMessage == errMsg {
		return nil
	}

	now := time.Now()
	if rec.State != newState {
		rec.State = newState
		rec.LastTransitionTime = now
	}
	rec.ErrorMessage = errMsg
	rec.UpdatedAt = now
	return nil
}
