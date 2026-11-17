// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicolegacy

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/dispatchguard"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type overlappingPowerClient struct {
	nicoapi.Client
	mu      sync.Mutex
	sources map[string]bool
	entered [2]chan struct{}
	release [2]chan struct{}
}

func (c *overlappingPowerClient) InsertHealthReportOverride(_ context.Context, _, source string) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	// Core stores one Replace report, superseding a crashed invocation too.
	clear(c.sources)
	c.sources[source] = true
	return nil
}

func (c *overlappingPowerClient) RemoveHealthReportOverride(_ context.Context, _, source string) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	delete(c.sources, source)
	return nil
}

func (c *overlappingPowerClient) UpdatePowerOption(ctx context.Context, _ string, state nicoapi.PowerState) error {
	index := 0
	if state == nicoapi.PowerStateOff {
		index = 1
	}
	close(c.entered[index])
	select {
	case <-c.release[index]:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func TestPreemptedPowerCleanupPreservesReplacementOverride(t *testing.T) {
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	client := &overlappingPowerClient{
		Client:  nicoapi.NewMockClient(),
		sources: make(map[string]bool),
		entered: [2]chan struct{}{make(chan struct{}), make(chan struct{})},
		release: [2]chan struct{}{make(chan struct{}), make(chan struct{})},
	}
	manager := New(client, 0, nil)
	target := common.Target{Type: devicetypes.ComponentTypeCompute, Identifiers: []string{"machine-1"}}
	var preempted atomic.Bool
	oldContext := dispatchguard.WithCheck(ctx, func(context.Context) error {
		if preempted.Load() {
			return errors.New("task preempted")
		}
		return nil
	})
	oldDone, replacementDone := make(chan error, 1), make(chan error, 1)
	go func() {
		oldDone <- manager.PowerControl(oldContext, target, operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn})
	}()
	select {
	case <-client.entered[0]:
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	preempted.Store(true)
	go func() {
		replacementDone <- manager.PowerControl(ctx, target, operations.PowerControlTaskInfo{Operation: operations.PowerOperationForcePowerOff})
	}()
	select {
	case <-client.entered[1]:
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	close(client.release[0])
	require.ErrorContains(t, <-oldDone, "task preempted")
	client.mu.Lock()
	remaining := len(client.sources)
	client.mu.Unlock()
	require.Equal(t, 1, remaining, "old cleanup must preserve the active replacement's report")
	close(client.release[1])
	require.NoError(t, <-replacementDone)
	client.mu.Lock()
	defer client.mu.Unlock()
	require.Empty(t, client.sources, "the replacement must clean up its own report")
}
