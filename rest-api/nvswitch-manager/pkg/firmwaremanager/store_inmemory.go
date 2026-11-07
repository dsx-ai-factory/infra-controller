// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"fmt"
	"sort"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
)

// Ensure InMemoryUpdateStore implements UpdateStore.
var _ UpdateStore = (*InMemoryUpdateStore)(nil)

// InMemoryUpdateStore provides an in-memory implementation of UpdateStore.
// All data is lost when the process exits.
type InMemoryUpdateStore struct {
	updates map[uuid.UUID]*FirmwareUpdate
	mu      sync.RWMutex
}

// NewInMemoryUpdateStore creates a new in-memory update store.
func NewInMemoryUpdateStore() *InMemoryUpdateStore {
	return &InMemoryUpdateStore{
		updates: make(map[uuid.UUID]*FirmwareUpdate),
	}
}

// Save persists a firmware update (insert or update).
func (s *InMemoryUpdateStore) Save(ctx context.Context, update *FirmwareUpdate) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	// Deep copy to avoid external mutations
	stored := *update
	stored.UpdatedAt = time.Now()
	s.updates[update.ID] = &stored

	return nil
}

// Get retrieves a firmware update by ID.
func (s *InMemoryUpdateStore) Get(ctx context.Context, id uuid.UUID) (*FirmwareUpdate, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	update, ok := s.updates[id]
	if !ok {
		return nil, fmt.Errorf("update not found: %w", ErrUpdateNotFound)
	}

	// Return a copy
	result := *update
	return &result, nil
}

// GetAll returns all firmware updates.
func (s *InMemoryUpdateStore) GetAll(ctx context.Context) ([]*FirmwareUpdate, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	var results []*FirmwareUpdate
	for _, update := range s.updates {
		copy := *update
		results = append(results, &copy)
	}

	// Sort by created_at descending (newest first)
	sort.Slice(results, func(i, j int) bool {
		return results[i].CreatedAt.After(results[j].CreatedAt)
	})

	return results, nil
}

// GetPendingUpdates returns up to `limit` updates that need processing.
func (s *InMemoryUpdateStore) GetPendingUpdates(ctx context.Context, limit int) ([]*FirmwareUpdate, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	var results []*FirmwareUpdate

	// First, collect QUEUED updates whose predecessor has completed
	var queuedUpdates []*FirmwareUpdate
	for _, update := range s.updates {
		if update.State == StateQueued {
			// Check if predecessor is complete (or no predecessor)
			if update.PredecessorID == nil {
				queuedUpdates = append(queuedUpdates, update)
			} else {
				pred, ok := s.updates[*update.PredecessorID]
				if ok && pred.State == StateCompleted {
					queuedUpdates = append(queuedUpdates, update)
				}
			}
		}
	}

	// Sort queued by sequence order
	sort.Slice(queuedUpdates, func(i, j int) bool {
		return queuedUpdates[i].SequenceOrder < queuedUpdates[j].SequenceOrder
	})

	// Add queued updates to results
	for _, update := range queuedUpdates {
		if len(results) >= limit {
			break
		}
		copy := *update
		results = append(results, &copy)
	}

	// If we still have room, add active (non-terminal, non-queued) updates
	if len(results) < limit {
		var activeUpdates []*FirmwareUpdate
		for _, update := range s.updates {
			if !update.State.IsTerminal() && update.State != StateQueued {
				activeUpdates = append(activeUpdates, update)
			}
		}

		// Sort by updated_at (oldest first)
		sort.Slice(activeUpdates, func(i, j int) bool {
			return activeUpdates[i].UpdatedAt.Before(activeUpdates[j].UpdatedAt)
		})

		for _, update := range activeUpdates {
			if len(results) >= limit {
				break
			}
			copy := *update
			results = append(results, &copy)
		}
	}

	return results, nil
}

// GetLatestBundleBySwitch returns firmware updates belonging to the most
// recent bundle_update_id for the given switch. For single-component
// updates (bundle_update_id IS NULL) it returns only the newest record.
func (s *InMemoryUpdateStore) GetLatestBundleBySwitch(ctx context.Context, switchUUID uuid.UUID) ([]*FirmwareUpdate, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	// Collect all updates for this switch
	var all []*FirmwareUpdate
	for _, update := range s.updates {
		if update.SwitchUUID == switchUUID {
			copy := *update
			all = append(all, &copy)
		}
	}

	if len(all) == 0 {
		return nil, nil
	}

	// Sort by created_at descending (newest first)
	sort.Slice(all, func(i, j int) bool {
		return all[i].CreatedAt.After(all[j].CreatedAt)
	})

	// Find the latest bundle_update_id (non-nil)
	var latestBundleID *uuid.UUID
	for _, u := range all {
		if u.BundleUpdateID != nil {
			latestBundleID = u.BundleUpdateID
			break
		}
	}

	var results []*FirmwareUpdate
	if latestBundleID != nil {
		for _, u := range all {
			if u.BundleUpdateID != nil && *u.BundleUpdateID == *latestBundleID {
				results = append(results, u)
			}
		}
	}

	// Also include the latest single-component update (no bundle_update_id)
	for _, u := range all {
		if u.BundleUpdateID == nil {
			results = append(results, u)
			break
		}
	}

	return results, nil
}

// GetActive returns the active (non-terminal) update for a switch/component pair.
func (s *InMemoryUpdateStore) GetActive(ctx context.Context, switchUUID uuid.UUID, component nvswitch.Component) (*FirmwareUpdate, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	for _, update := range s.updates {
		if update.SwitchUUID == switchUUID && update.Component == component && !update.State.IsTerminal() {
			copy := *update
			return &copy, nil
		}
	}

	return nil, nil
}

// GetAnyActiveForSwitch returns any active (non-terminal) update for a switch.
func (s *InMemoryUpdateStore) GetAnyActiveForSwitch(ctx context.Context, switchUUID uuid.UUID) (*FirmwareUpdate, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	for _, update := range s.updates {
		if update.SwitchUUID == switchUUID && !update.State.IsTerminal() {
			copy := *update
			return &copy, nil
		}
	}

	return nil, nil
}

// CancelRemainingInBundle cancels all QUEUED updates in a bundle that come after
// the specified sequence order.
func (s *InMemoryUpdateStore) CancelRemainingInBundle(ctx context.Context, bundleUpdateID uuid.UUID, afterSequence int, failedComponent nvswitch.Component) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	cancelled := 0
	for _, update := range s.updates {
		if update.BundleUpdateID != nil &&
			*update.BundleUpdateID == bundleUpdateID &&
			update.SequenceOrder > afterSequence &&
			update.State == StateQueued {

			update.State = StateCancelled
			update.ErrorMessage = fmt.Sprintf("cancelled due to %s failure", failedComponent)
			update.UpdatedAt = time.Now()
			cancelled++
		}
	}

	return cancelled, nil
}

// Delete removes a firmware update by ID.
func (s *InMemoryUpdateStore) Delete(ctx context.Context, id uuid.UUID) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if _, ok := s.updates[id]; !ok {
		return fmt.Errorf("update not found: %w", ErrUpdateNotFound)
	}

	delete(s.updates, id)
	return nil
}
