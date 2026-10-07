// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package detector translates NICo leakage polling results into event-rule
// envelopes.
package detector

import (
	"context"
	"errors"
	"fmt"
	"net/url"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/google/uuid"
)

// SourceName is the stable logical identity shared by every instance of the
// NICo leakage detector.
const SourceName = "nico_core_leakage"

// Reader exposes the NICo leakage queries used by Detector.
type Reader interface {
	GetLeakingMachineIds(context.Context) ([]string, error)
	GetLeakingSwitchIds(context.Context) ([]string, error)
}

// IngestFunc synchronously accepts one constructed leakage event.
type IngestFunc func(context.Context, eventrule.Envelope) error

func newOccurrenceKey(
	componentType flowtypes.ComponentType,
	externalID string,
	suffix string,
) string {
	return fmt.Sprintf(
		"%s/%s/%s",
		strings.ToLower(string(componentType)),
		url.PathEscape(externalID),
		suffix,
	)
}

type occurrence struct {
	externalID string
	sourceKey  string
}

func (o occurrence) envelope(
	componentType flowtypes.ComponentType,
	observedAt time.Time,
) eventrule.Envelope {
	return eventrule.Envelope{
		Key:      eventrule.EventKey{SourceKey: o.sourceKey},
		Type:     leakage.TypeHardwareLeakDetected,
		Severity: eventrule.SeverityCritical,
		Resource: eventrule.Resource{
			Kind:              eventrule.ResourceKindComponent,
			ExternalID:        o.externalID,
			ComponentTypeHint: componentType,
		},
		ObservedAt: observedAt,
	}
}

// Detector polls NICo and retains one source key while a component remains in
// the leaking result set. If a component clears and later leaks again, it gets
// a new occurrence key. NICo currently exposes component identities but no
// upstream leak-occurrence identity, so the detector generates and retains the
// occurrence suffix locally.
type Detector struct {
	reader        Reader
	mu            sync.Mutex
	active        map[flowtypes.ComponentType]map[string]string
	now           func() time.Time
	newOccurrence func() string
}

// New constructs a NICo leakage detector.
func New(reader Reader) (*Detector, error) {
	if reader == nil {
		return nil, fmt.Errorf("leakage reader is required")
	}

	return &Detector{
		reader:        reader,
		active:        make(map[flowtypes.ComponentType]map[string]string),
		now:           time.Now,
		newOccurrence: uuid.NewString,
	}, nil
}

// Collect reads every currently leaking machine and switch and ingests their
// events one at a time. Terminal per-event failures are accumulated while
// collection continues. Successful source queries update occurrence state
// independently, so an outage in one query does not clear or suppress results
// from the other.
func (d *Detector) Collect(ctx context.Context, ingest IngestFunc) error {
	if d == nil || d.reader == nil {
		return fmt.Errorf("leakage detector is not configured")
	}

	if ingest == nil {
		return fmt.Errorf("event ingestor is required")
	}

	var collectErr error

	sources := []struct {
		componentType flowtypes.ComponentType
		load          func(context.Context) ([]string, error)
	}{
		{componentType: flowtypes.ComponentTypeCompute, load: d.reader.GetLeakingMachineIds},
		{componentType: flowtypes.ComponentTypeNVSwitch, load: d.reader.GetLeakingSwitchIds},
	}

	for _, source := range sources {
		ids, err := source.load(ctx)
		if err != nil {
			collectErr = errors.Join(
				collectErr,
				fmt.Errorf("collect leaking %s components: %w", source.componentType, err),
			)
			continue
		}

		ids = normalizeExternalIDs(ids)

		err = d.ingestEvents(
			ctx,
			source.componentType,
			ids,
			ingest,
		)
		if err != nil {
			if errors.Is(err, eventrule.ErrTerminalProcessing) {
				collectErr = errors.Join(collectErr, err)

				continue
			}

			return errors.Join(collectErr, err)
		}
	}

	return collectErr
}

func (d *Detector) ingestEvents(
	ctx context.Context,
	componentType flowtypes.ComponentType,
	externalIDs []string,
	ingest IngestFunc,
) error {
	var terminalErr error

	occurrences := d.buildOccurrences(componentType, externalIDs)
	observedAt := d.now()

	for _, occurrence := range occurrences {
		envelope := occurrence.envelope(componentType, observedAt)
		err := ingest(ctx, envelope)
		if err != nil {
			// A dependency may return its own error after cancellation. Preserve the
			// context error so the scheduled job is recognized as canceled.
			if ctxErr := ctx.Err(); ctxErr != nil {
				return ctxErr
			}

			if !errors.Is(err, eventrule.ErrTerminalProcessing) {
				return err
			}

			err = fmt.Errorf(
				"ingest leaking %s component %q: %w",
				componentType,
				occurrence.externalID,
				err,
			)
			terminalErr = errors.Join(terminalErr, err)
		}
	}

	return terminalErr
}

func normalizeExternalIDs(externalIDs []string) []string {
	uniqueIDs := make(map[string]struct{}, len(externalIDs))
	for _, externalID := range externalIDs {
		externalID = strings.TrimSpace(externalID)
		if externalID == "" {
			continue
		}

		uniqueIDs[externalID] = struct{}{}
	}

	ids := make([]string, 0, len(uniqueIDs))
	for externalID := range uniqueIDs {
		ids = append(ids, externalID)
	}

	slices.Sort(ids)

	return ids
}

// buildOccurrences returns a stable source key for each component throughout
// one continuous leakage occurrence. An identity remains active while it is
// present in successive successful polls, so repeated events can be
// deduplicated. A successful poll that omits the identity ends the occurrence;
// if it appears again later, it receives a new source key. Load failures do not
// call this method and therefore do not end active occurrences.
func (d *Detector) buildOccurrences(
	componentType flowtypes.ComponentType,
	externalIDs []string,
) []occurrence {
	current := make(map[string]string, len(externalIDs))
	occurrences := make([]occurrence, 0, len(externalIDs))

	d.mu.Lock()
	defer d.mu.Unlock()

	previous := d.active[componentType]

	for _, externalID := range externalIDs {
		key, exists := previous[externalID]
		if !exists {
			key = newOccurrenceKey(componentType, externalID, d.newOccurrence())
		}

		current[externalID] = key
		occurrences = append(occurrences, occurrence{
			externalID: externalID,
			sourceKey:  key,
		})
	}

	d.active[componentType] = current

	return occurrences
}
