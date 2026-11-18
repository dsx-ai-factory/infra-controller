// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package leakdetection

import (
	"context"
	"errors"
	"sync/atomic"
	"testing"

	schedtypes "github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/types"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/stretchr/testify/require"
)

func TestJob_Run(t *testing.T) {
	runErr := errors.New("detector failed")

	tests := map[string]struct {
		runner  *countingRunner
		wantErr error
	}{
		"runs detector": {
			runner: &countingRunner{},
		},
		"returns detector error": {
			runner:  &countingRunner{err: runErr},
			wantErr: runErr,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			job, err := newJob(test.runner)
			require.NoError(t, err)

			err = job.Run(context.Background(), schedtypes.Event{})

			if test.wantErr != nil {
				require.ErrorIs(t, err, test.wantErr)
			} else {
				require.NoError(t, err)
			}

			require.Equal(t, int32(1), test.runner.runs.Load())
		})
	}
}

func TestNewJob(t *testing.T) {
	tests := map[string]struct {
		detector detectorRunner
		wantErr  string
	}{
		"constructs job": {
			detector: &countingRunner{},
		},
		"rejects nil detector": {
			wantErr: "leakage detector is required",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			job, err := newJob(test.detector)

			if test.wantErr != "" {
				require.EqualError(t, err, test.wantErr)
				require.Nil(t, job)

				return
			}

			require.NoError(t, err)
			require.NotNil(t, job)
			require.Equal(t, test.detector, job.detector)
		})
	}
}

func TestReconcilingReader_GetLeakingRackIds(t *testing.T) {
	loadErr := errors.New("rack load failed")

	tests := map[string]struct {
		ids     []string
		loadErr error
	}{
		"returns successful load without component reconciliation": {
			ids: []string{"rack-1", "rack-2"},
		},
		"returns load failure": {
			loadErr: loadErr,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			reconciled := false
			reader := reconcilingReader{
				reader: &stubLeakReader{
					rackIDs: test.ids,
					rackErr: test.loadErr,
				},
				reconcile: func(context.Context, devicetypes.ComponentType, []string) {
					reconciled = true
				},
			}

			ids, err := reader.GetLeakingRackIds(context.Background())

			if test.loadErr != nil {
				require.ErrorIs(t, err, test.loadErr)
			} else {
				require.NoError(t, err)
				require.Equal(t, test.ids, ids)
			}
			require.False(t, reconciled)
		})
	}
}

func TestReconcilingReader_GetLeakingMachineIds(t *testing.T) {
	loadErr := errors.New("machine load failed")

	tests := map[string]struct {
		ids            []string
		loadErr        error
		wantReconciled bool
	}{
		"reconciles successful load": {
			ids:            []string{"machine-1", "machine-2"},
			wantReconciled: true,
		},
		"does not reconcile failed load": {
			loadErr: loadErr,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			var reconciled bool
			var reconciledType devicetypes.ComponentType
			var reconciledIDs []string

			reader := reconcilingReader{
				reader: &stubLeakReader{
					machineIDs: test.ids,
					machineErr: test.loadErr,
				},
				reconcile: func(
					_ context.Context,
					componentType devicetypes.ComponentType,
					ids []string,
				) {
					reconciled = true
					reconciledType = componentType
					reconciledIDs = ids
				},
			}

			ids, err := reader.GetLeakingMachineIds(context.Background())

			if test.loadErr != nil {
				require.ErrorIs(t, err, test.loadErr)
			} else {
				require.NoError(t, err)
				require.Equal(t, test.ids, ids)
			}

			require.Equal(t, test.wantReconciled, reconciled)
			if test.wantReconciled {
				require.Equal(t, devicetypes.ComponentTypeCompute, reconciledType)
				require.Equal(t, test.ids, reconciledIDs)
			}
		})
	}
}

func TestReconcilingReader_GetLeakingSwitchIds(t *testing.T) {
	loadErr := errors.New("switch load failed")

	tests := map[string]struct {
		ids            []string
		loadErr        error
		wantReconciled bool
	}{
		"reconciles successful load": {
			ids:            []string{"switch-1", "switch-2"},
			wantReconciled: true,
		},
		"does not reconcile failed load": {
			loadErr: loadErr,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			var reconciled bool
			var reconciledType devicetypes.ComponentType
			var reconciledIDs []string

			reader := reconcilingReader{
				reader: &stubLeakReader{
					switchIDs: test.ids,
					switchErr: test.loadErr,
				},
				reconcile: func(
					_ context.Context,
					componentType devicetypes.ComponentType,
					ids []string,
				) {
					reconciled = true
					reconciledType = componentType
					reconciledIDs = ids
				},
			}

			ids, err := reader.GetLeakingSwitchIds(context.Background())

			if test.loadErr != nil {
				require.ErrorIs(t, err, test.loadErr)
			} else {
				require.NoError(t, err)
				require.Equal(t, test.ids, ids)
			}

			require.Equal(t, test.wantReconciled, reconciled)
			if test.wantReconciled {
				require.Equal(t, devicetypes.ComponentTypeNVSwitch, reconciledType)
				require.Equal(t, test.ids, reconciledIDs)
			}
		})
	}
}

type countingRunner struct {
	runs atomic.Int32
	err  error
}

func (r *countingRunner) Run(context.Context) error {
	r.runs.Add(1)

	return r.err
}

type stubLeakReader struct {
	rackIDs    []string
	machineIDs []string
	switchIDs  []string
	rackErr    error
	machineErr error
	switchErr  error
}

func (r *stubLeakReader) GetLeakingRackIds(context.Context) ([]string, error) {
	return r.rackIDs, r.rackErr
}

func (r *stubLeakReader) GetLeakingMachineIds(context.Context) ([]string, error) {
	return r.machineIDs, r.machineErr
}

func (r *stubLeakReader) GetLeakingSwitchIds(context.Context) ([]string, error) {
	return r.switchIDs, r.switchErr
}
