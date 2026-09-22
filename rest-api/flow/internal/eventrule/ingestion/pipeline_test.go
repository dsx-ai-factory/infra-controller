// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

func TestDefaultConfig(t *testing.T) {
	config := DefaultConfig()

	require.Equal(t, time.Second, config.RetryDelay)
	require.Equal(t, defaultMaxDeliveryAttempts, config.MaxAttempts)
	require.NoError(t, config.Validate())
}

func TestNew(t *testing.T) {
	tests := map[string]struct {
		sink    Sink
		config  Config
		wantErr string
	}{
		"valid": {
			sink:   &recordingSink{},
			config: DefaultConfig(),
		},
		"missing sink": {
			config:  DefaultConfig(),
			wantErr: "event sink is required",
		},
		"invalid retry delay": {
			sink:    &recordingSink{},
			config:  Config{},
			wantErr: "event delivery retry delay must be positive",
		},
		"invalid max attempts": {
			sink: &recordingSink{},
			config: Config{
				RetryDelay: time.Second,
			},
			wantErr: "event delivery max attempts must be positive",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			pipeline, err := New(test.sink, test.config)
			if test.wantErr != "" {
				require.EqualError(t, err, test.wantErr)
				require.Nil(t, pipeline)

				return
			}

			require.NoError(t, err)
			require.NotNil(t, pipeline)
		})
	}
}

func TestPipeline_RegisterSource(t *testing.T) {
	tests := map[string]struct {
		name        string
		twice       bool
		nilPipeline bool
		wantErr     string
	}{
		"valid": {
			name: "leakage",
		},
		"invalid name": {
			name:    "Leakage",
			wantErr: "event source name",
		},
		"duplicate": {
			name:    "leakage",
			twice:   true,
			wantErr: "already registered",
		},
		"nil pipeline": {
			name:        "leakage",
			nilPipeline: true,
			wantErr:     "event ingestion pipeline is nil",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			pipeline := newTestPipeline(t, &recordingSink{})
			if test.nilPipeline {
				pipeline = nil
			}

			source, err := pipeline.RegisterSource(test.name)
			if err == nil && test.twice {
				source, err = pipeline.RegisterSource(test.name)
			}

			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				require.Nil(t, source)

				return
			}

			require.NoError(t, err)
			require.NotNil(t, source)
		})
	}
}

func TestSource_Ingest(t *testing.T) {
	now := time.Date(2026, 8, 26, 12, 0, 0, 0, time.UTC)
	envelope := eventrule.Envelope{
		Key: eventrule.EventKey{
			SourceName: "collector_supplied_name",
			SourceKey:  "occurrence-1",
		},
		Type:       "test.event",
		Resource:   eventrule.Resource{Kind: eventrule.ResourceKindRack, ID: uuid.New()},
		ObservedAt: now,
	}
	storeErr := errors.New("store unavailable")
	terminalErr := errors.Join(eventrule.ErrTerminalProcessing, errors.New("invalid event"))

	tests := map[string]struct {
		sourceName      string
		sinkErrors      []error
		envelope        eventrule.Envelope
		nilSource       bool
		cancelCtx       bool
		wantErrIs       error
		wantErrContains string
		wantDelivered   int
		wantKeys        []eventrule.EventKey
	}{
		"applies registered source name": {
			sourceName:    "registered_source",
			envelope:      envelope,
			wantDelivered: 1,
			wantKeys: []eventrule.EventKey{{
				SourceName: "registered_source",
				SourceKey:  "occurrence-1",
			}},
		},
		"retries transient failure with the same key": {
			sinkErrors:    []error{storeErr},
			envelope:      envelope,
			wantDelivered: 2,
			wantKeys: []eventrule.EventKey{
				{SourceName: "leakage", SourceKey: "occurrence-1"},
				{SourceName: "leakage", SourceKey: "occurrence-1"},
			},
		},
		"bounds persistent nonterminal failures": {
			sinkErrors:      []error{storeErr, storeErr, storeErr},
			envelope:        envelope,
			wantErrIs:       storeErr,
			wantErrContains: "after 3 attempts",
			wantDelivered:   3,
		},
		"does not retry terminal failure": {
			sinkErrors:    []error{terminalErr},
			envelope:      envelope,
			wantErrIs:     eventrule.ErrTerminalProcessing,
			wantDelivered: 1,
		},
		"rejects invalid envelope": {
			wantErrContains: `event source "leakage"`,
		},
		"rejects nil source": {
			envelope:        envelope,
			nilSource:       true,
			wantErrContains: "event source is nil",
		},
		"does not deliver after cancellation": {
			envelope:  envelope,
			cancelCtx: true,
			wantErrIs: context.Canceled,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			sink := &recordingSink{errors: test.sinkErrors}

			var source *Source
			if !test.nilSource {
				pipeline := newTestPipeline(t, sink)
				sourceName := test.sourceName
				if sourceName == "" {
					sourceName = "leakage"
				}

				var err error
				source, err = pipeline.RegisterSource(sourceName)
				require.NoError(t, err)
			}

			ctx := context.Background()
			if test.cancelCtx {
				var cancel context.CancelFunc
				ctx, cancel = context.WithCancel(ctx)
				cancel()
			}

			err := source.Ingest(ctx, test.envelope)

			if test.wantErrIs != nil {
				require.ErrorIs(t, err, test.wantErrIs)
			} else if test.wantErrContains == "" {
				require.NoError(t, err)
			}

			if test.wantErrContains != "" {
				require.ErrorContains(t, err, test.wantErrContains)
			}

			delivered := sink.delivered()
			require.Len(t, delivered, test.wantDelivered)
			for i, wantKey := range test.wantKeys {
				require.Equal(t, wantKey, delivered[i].Key)
			}
		})
	}
}

func newTestPipeline(t *testing.T, sink Sink) *Pipeline {
	t.Helper()

	config := DefaultConfig()
	config.RetryDelay = time.Millisecond
	pipeline, err := New(sink, config)
	require.NoError(t, err)

	return pipeline
}

type recordingSink struct {
	mu        sync.Mutex
	envelopes []eventrule.Envelope
	errors    []error
}

func (s *recordingSink) Process(ctx context.Context, envelope eventrule.Envelope) error {
	if err := ctx.Err(); err != nil {
		return err
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	s.envelopes = append(s.envelopes, envelope.Clone())

	if len(s.errors) == 0 {
		return nil
	}

	err := s.errors[0]
	s.errors = s.errors[1:]

	return err
}

func (s *recordingSink) delivered() []eventrule.Envelope {
	s.mu.Lock()
	defer s.mu.Unlock()

	cloned := make([]eventrule.Envelope, len(s.envelopes))
	for i := range s.envelopes {
		cloned[i] = s.envelopes[i].Clone()
	}

	return cloned
}
