// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import (
	"fmt"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	eventrulecodec "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/codec"
)

// EventTo converts a durable domain event to its database representation.
func EventTo(event *eventrule.Event) (*dbmodel.Event, error) {
	if err := event.Validate(); err != nil {
		return nil, err
	}

	policy, err := eventrulecodec.MarshalPolicy(event.EffectivePolicy)
	if err != nil {
		return nil, fmt.Errorf("encode effective policy: %w", err)
	}

	return &dbmodel.Event{
		ID:              event.ID,
		SourceName:      event.Key.SourceName,
		SourceKey:       event.Key.SourceKey,
		EventType:       string(event.Type),
		ResourceID:      event.Resource.ID,
		ResourceKind:    string(event.Resource.Kind),
		AppliedRuleID:   event.AppliedRuleID,
		EffectivePolicy: policy,
		Summary:         event.Summary,
		Observations:    event.Observations,
		CreatedAt:       event.CreatedAt,
		LastObservedAt:  event.LastObservedAt,
	}, nil
}

// EventFrom converts a database event into a validated domain event.
func EventFrom(persisted *dbmodel.Event) (*eventrule.Event, error) {
	if persisted == nil {
		return nil, nil
	}

	policy, err := eventrulecodec.UnmarshalPolicy(persisted.EffectivePolicy)
	if err != nil {
		return nil, fmt.Errorf("%w: decode effective policy: %w", eventrule.ErrInvalidPersistedEvent, err)
	}

	event := &eventrule.Event{
		ID: persisted.ID,
		Key: eventrule.EventKey{
			SourceName: persisted.SourceName,
			SourceKey:  persisted.SourceKey,
		},
		Type:            eventrule.Type(persisted.EventType),
		Resource:        eventrule.ResourceIdentity{ID: persisted.ResourceID, Kind: eventrule.ResourceKind(persisted.ResourceKind)},
		AppliedRuleID:   persisted.AppliedRuleID,
		EffectivePolicy: policy,
		Summary:         persisted.Summary,
		Observations:    persisted.Observations,
		CreatedAt:       timeFromPersistence(persisted.CreatedAt),
		LastObservedAt:  timeFromPersistence(persisted.LastObservedAt),
	}

	if err := event.Validate(); err != nil {
		return nil, fmt.Errorf("%w: %w", eventrule.ErrInvalidPersistedEvent, err)
	}

	return event, nil
}
