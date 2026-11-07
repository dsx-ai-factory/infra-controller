// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package readiness

import (
	"context"
	"sync"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// MemReader is an in-memory StatusReader intended for unit tests of code
// that depends on a Gate. It mirrors nicoapi.NewMockClient's style:
// production callers should always use NewDBReader, but having the
// in-memory reader exported lets test packages outside `readiness` build
// realistic gates without spinning up a DB.
//
// MemReader is goroutine-safe so tests can mutate state mid-poll.
type MemReader struct {
	mu       sync.Mutex
	statuses map[string]*types.ComponentOperationStatus
	hosts    map[string][]string
}

// NewMemReader returns an empty in-memory StatusReader.
func NewMemReader() *MemReader {
	return &MemReader{
		statuses: map[string]*types.ComponentOperationStatus{},
		hosts:    map[string][]string{},
	}
}

// SetStatus records the persisted ComponentOperationStatus for an external (Core)
// component ID. Pass nil to clear.
func (r *MemReader) SetStatus(externalID string, s *types.ComponentOperationStatus) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if s == nil {
		delete(r.statuses, externalID)
		return
	}
	r.statuses[externalID] = s
}

// SetRackHosts records the host (compute) external IDs that belong to a
// given Core rack ID.
func (r *MemReader) SetRackHosts(rackID string, hostIDs []string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	cp := make([]string, len(hostIDs))
	copy(cp, hostIDs)
	r.hosts[rackID] = cp
}

// GetStatusesByExternalIDs implements StatusReader.
func (r *MemReader) GetStatusesByExternalIDs(_ context.Context, externalIDs []string) (map[string]*types.ComponentOperationStatus, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make(map[string]*types.ComponentOperationStatus, len(externalIDs))
	for _, id := range externalIDs {
		if s, ok := r.statuses[id]; ok {
			out[id] = s
		}
	}
	return out, nil
}

// GetHostExternalIDsByRackIDs implements StatusReader.
func (r *MemReader) GetHostExternalIDsByRackIDs(_ context.Context, rackIDs []string) (map[string][]string, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make(map[string][]string, len(rackIDs))
	for _, id := range rackIDs {
		if h, ok := r.hosts[id]; ok {
			cp := make([]string, len(h))
			copy(cp, h)
			out[id] = cp
		}
	}
	return out, nil
}

var _ StatusReader = (*MemReader)(nil)

// GetStatusesByManagementMACs uses MAC-keyed statuses for compute and MAC-keyed
// host lists for rack devices. Tests seed these with SetStatus/SetRackHosts.
func (r *MemReader) GetStatusesByManagementMACs(_ context.Context, componentType devicetypes.ComponentType, macs []string) (map[string][]*types.ComponentOperationStatus, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make(map[string][]*types.ComponentOperationStatus, len(macs))
	for _, mac := range macs {
		if componentType == devicetypes.ComponentTypeCompute {
			out[mac] = []*types.ComponentOperationStatus{r.statuses[mac]}
		} else {
			for _, host := range r.hosts[mac] {
				out[mac] = append(out[mac], r.statuses[host])
			}
		}
	}
	return out, nil
}
