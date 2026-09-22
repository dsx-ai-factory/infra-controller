// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package detector

import (
	"context"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/stretchr/testify/require"
)

func TestDetector_Collect(t *testing.T) {
	t.Run("ingests one event per leaking component", func(t *testing.T) {
		computeObservedAt := time.Date(2026, 8, 26, 12, 0, 0, 0, time.UTC)
		switchObservedAt := computeObservedAt.Add(time.Second)
		reader := &leakReader{
			machines: []string{
				"",
				"machine-2",
				"machine-1",
				" machine-1",
				"machine-2 ",
				"\t",
			},
			switches: []string{"switch-1"},
		}
		detector := newTestDetector(t, reader, computeObservedAt)

		observedAts := []time.Time{computeObservedAt, switchObservedAt}
		nextObservedAt := 0
		detector.now = func() time.Time {
			observedAt := observedAts[nextObservedAt]
			nextObservedAt++

			return observedAt
		}

		envelopes, err := collectEnvelopes(context.Background(), detector)

		require.NoError(t, err)
		require.Len(t, envelopes, 3)
		require.Equal(t, []string{"machine-1", "machine-2", "switch-1"}, []string{
			envelopes[0].Resource.ExternalID,
			envelopes[1].Resource.ExternalID,
			envelopes[2].Resource.ExternalID,
		})
		require.Equal(t, []flowtypes.ComponentType{
			flowtypes.ComponentTypeCompute,
			flowtypes.ComponentTypeCompute,
			flowtypes.ComponentTypeNVSwitch,
		}, []flowtypes.ComponentType{
			envelopes[0].Resource.ComponentTypeHint,
			envelopes[1].Resource.ComponentTypeHint,
			envelopes[2].Resource.ComponentTypeHint,
		})
		for _, envelope := range envelopes {
			require.Empty(t, envelope.Key.SourceName)
			require.NotEmpty(t, envelope.Key.SourceKey)
			require.Equal(t, leakage.TypeHardwareLeakDetected, envelope.Type)
			require.Equal(t, eventrule.SeverityCritical, envelope.Severity)
			require.Equal(t, eventrule.ResourceKindComponent, envelope.Resource.Kind)
		}
		require.Equal(t, []time.Time{
			computeObservedAt,
			computeObservedAt,
			switchObservedAt,
		}, []time.Time{
			envelopes[0].ObservedAt,
			envelopes[1].ObservedAt,
			envelopes[2].ObservedAt,
		})
	})

	t.Run("retains source key for one occurrence", func(t *testing.T) {
		now := time.Date(2026, 8, 26, 12, 0, 0, 0, time.UTC)
		reader := &leakReader{machines: []string{"machine/1"}}
		detector := newTestDetector(t, reader, now)

		first, err := collectEnvelopes(context.Background(), detector)
		require.NoError(t, err)

		second, err := collectEnvelopes(context.Background(), detector)
		require.NoError(t, err)
		require.Equal(t, first[0].Key.SourceKey, second[0].Key.SourceKey)
		require.Equal(t, "compute/machine%2F1/occurrence-1", first[0].Key.SourceKey)

		reader.machines = nil
		_, err = collectEnvelopes(context.Background(), detector)
		require.NoError(t, err)

		reader.machines = []string{"machine/1"}

		reappeared, err := collectEnvelopes(context.Background(), detector)
		require.NoError(t, err)
		require.Equal(t, "compute/machine%2F1/occurrence-2", reappeared[0].Key.SourceKey)
	})

	t.Run("ingests successful source after another source fails", func(t *testing.T) {
		reader := &leakReader{
			machineErr: errors.New("machine query unavailable"),
			switches:   []string{"switch-1"},
		}
		detector := newTestDetector(t, reader, time.Now())

		envelopes, err := collectEnvelopes(context.Background(), detector)

		require.ErrorContains(t, err, "machine query unavailable")
		require.Len(t, envelopes, 1)
		require.Equal(t, "switch-1", envelopes[0].Resource.ExternalID)
	})

	t.Run("continues after terminal per-event failures", func(t *testing.T) {
		terminalErr := errors.Join(
			eventrule.ErrTerminalProcessing,
			errors.New("component not found"),
		)
		detector := newTestDetector(t, &leakReader{
			machines: []string{"machine-1", "machine-2"},
			switches: []string{"switch-1"},
		}, time.Now())

		var ingested []string
		err := detector.Collect(
			context.Background(),
			func(_ context.Context, envelope eventrule.Envelope) error {
				externalID := envelope.Resource.ExternalID
				ingested = append(ingested, externalID)
				if externalID == "machine-1" || externalID == "switch-1" {
					return terminalErr
				}

				return nil
			},
		)

		require.ErrorIs(t, err, eventrule.ErrTerminalProcessing)
		require.ErrorContains(t, err, `component "machine-1"`)
		require.ErrorContains(t, err, `component "switch-1"`)
		require.Equal(t, []string{"machine-1", "machine-2", "switch-1"}, ingested)
	})

	t.Run("stops on nonterminal ingestion failure", func(t *testing.T) {
		terminalErr := errors.Join(
			eventrule.ErrTerminalProcessing,
			errors.New("component not found"),
		)
		ingestErr := errors.New("sink unavailable")
		detector := newTestDetector(t, &leakReader{
			machines: []string{"machine-1", "machine-2"},
			switches: []string{"switch-1"},
		}, time.Now())

		ingested := 0
		err := detector.Collect(
			context.Background(),
			func(context.Context, eventrule.Envelope) error {
				ingested++
				if ingested == 1 {
					return terminalErr
				}

				return ingestErr
			},
		)

		require.ErrorIs(t, err, ingestErr)
		require.NotErrorIs(t, err, eventrule.ErrTerminalProcessing)
		require.Equal(t, 2, ingested)
	})

	t.Run("stops when ingestion cancels the context", func(t *testing.T) {
		detector := newTestDetector(t, &leakReader{
			machines: []string{"machine-1", "machine-2"},
			switches: []string{"switch-1"},
		}, time.Now())
		ctx, cancel := context.WithCancel(context.Background())

		ingested := 0
		err := detector.Collect(
			ctx,
			func(context.Context, eventrule.Envelope) error {
				ingested++
				cancel()

				return errors.New("inventory unavailable")
			},
		)

		require.ErrorIs(t, err, context.Canceled)
		require.Equal(t, 1, ingested)
	})

	t.Run("rejects missing ingestor", func(t *testing.T) {
		detector := newTestDetector(t, &leakReader{}, time.Now())

		require.EqualError(
			t,
			detector.Collect(context.Background(), nil),
			"event ingestor is required",
		)
	})
}

func collectEnvelopes(
	ctx context.Context,
	detector *Detector,
) ([]eventrule.Envelope, error) {
	var envelopes []eventrule.Envelope

	err := detector.Collect(ctx, func(_ context.Context, envelope eventrule.Envelope) error {
		envelopes = append(envelopes, envelope.Clone())

		return nil
	})

	return envelopes, err
}

func newTestDetector(t *testing.T, reader Reader, now time.Time) *Detector {
	t.Helper()

	detector, err := New(reader)
	require.NoError(t, err)

	detector.now = func() time.Time { return now }

	next := 0
	detector.newOccurrence = func() string {
		next++
		return fmt.Sprintf("occurrence-%d", next)
	}

	return detector
}

type leakReader struct {
	machines   []string
	switches   []string
	machineErr error
	switchErr  error
}

func (r *leakReader) GetLeakingMachineIds(context.Context) ([]string, error) {
	return r.machines, r.machineErr
}

func (r *leakReader) GetLeakingSwitchIds(context.Context) ([]string, error) {
	return r.switches, r.switchErr
}
