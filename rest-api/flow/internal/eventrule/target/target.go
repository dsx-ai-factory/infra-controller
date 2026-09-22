// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package target resolves event-rule task strategies into canonical action
// targets.
package target

import (
	"context"
	"errors"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
)

// ErrUnresolvable identifies an invalid target request or resolver result.
// Retrying without changing the request, registered resolvers, or inventory
// state cannot resolve it.
var ErrUnresolvable = errors.New("event target cannot be resolved")

// Target identifies one canonical target for an action.
type Target struct {
	Kind   eventrule.ResourceKind
	ID     uuid.UUID
	RackID uuid.UUID
}

// ResolveRequest contains the durable event classification, reconstructed
// resource, and strategy needed to resolve concrete targets while planning.
type ResolveRequest struct {
	EventType eventrule.Type
	Resource  eventrule.ResolvedResource
	Strategy  eventrule.TargetStrategy
}

// Resolver resolves concrete targets for task actions.
type Resolver interface {
	Resolve(context.Context, ResolveRequest) ([]Target, error)
}

// Validate checks that the target has a supported kind and canonical identity.
func (t Target) Validate() error {
	if err := t.Kind.Validate(); err != nil {
		return err
	}
	if t.ID == uuid.Nil {
		return fmt.Errorf("target id is required")
	}
	if t.RackID == uuid.Nil {
		return fmt.Errorf("target rack id is required")
	}
	if t.Kind == eventrule.ResourceKindRack && t.ID != t.RackID {
		return fmt.Errorf("rack target id must equal rack id")
	}
	return nil
}
