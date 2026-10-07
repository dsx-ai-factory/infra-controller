// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"fmt"
	"time"

	"github.com/google/uuid"
)

const maxRuleNameLength = 128

// RuleOrigin identifies whether a rule is persisted or defined by code.
type RuleOrigin string

const (
	RuleOriginPersisted RuleOrigin = "persisted"
	RuleOriginBuiltIn   RuleOrigin = "built_in"
)

// Validate checks that the rule origin is supported.
func (o RuleOrigin) Validate() error {
	switch o {
	case RuleOriginPersisted, RuleOriginBuiltIn:
		return nil
	default:
		return fmt.Errorf("unknown rule origin %q", o)
	}
}

// Policy defines how a selected event rule responds to an event. EventType
// remains on the owning rule because it controls selection.
type Policy struct {
	Actions []Action
}

// Clone returns an independent copy of the policy and its mutable data.
func (p Policy) Clone() Policy {
	cloned := p
	cloned.Actions = CloneActions(p.Actions)

	return cloned
}

// Validate checks each present action and action-name uniqueness. An empty
// policy is structurally valid; owning types decide whether they require an
// action.
func (p Policy) Validate() error {
	return ValidateActions(p.Actions)
}

// Rule is the in-memory domain type for a persisted or built-in event policy.
type Rule struct {
	ID          uuid.UUID
	Origin      RuleOrigin
	Name        string
	Description string
	Enabled     bool
	EventType   Type
	Policy
	CreatedAt time.Time
	UpdatedAt time.Time
}

// RuleMetadata contains the independently mutable descriptive fields of a rule.
type RuleMetadata struct {
	Name        string
	Description string
}

// RuleCreate contains the caller-provided fields for a persisted rule.
type RuleCreate struct {
	Metadata  RuleMetadata
	EventType Type
	Policy    Policy
}

// Validate checks fields supplied when creating a persisted rule.
func (c RuleCreate) Validate() error {
	if err := c.Metadata.Validate(); err != nil {
		return err
	}

	if err := c.EventType.Validate(); err != nil {
		return err
	}
	if len(c.Policy.Actions) == 0 {
		return fmt.Errorf("actions are required")
	}

	return c.Policy.Validate()
}

// Validate checks mutable rule metadata.
func (m RuleMetadata) Validate() error {
	if err := validateRequiredString("event rule name", m.Name); err != nil {
		return err
	}
	if len(m.Name) > maxRuleNameLength {
		return fmt.Errorf("event rule name exceeds %d characters", maxRuleNameLength)
	}
	return validateOptionalString("event rule description", m.Description)
}

// Clone returns an independent copy of the rule and its mutable policy data.
func (r Rule) Clone() Rule {
	cloned := r
	cloned.Policy = r.Policy.Clone()
	return cloned
}

// Validate checks rule metadata and policy.
func (r *Rule) Validate() error {
	if r == nil {
		return fmt.Errorf("event rule is nil")
	}
	if r.ID == uuid.Nil {
		return fmt.Errorf("event rule id is required")
	}
	if err := r.Origin.Validate(); err != nil {
		return err
	}
	if r.Origin == RuleOriginBuiltIn && !r.Enabled {
		return fmt.Errorf("built-in event rule must be enabled")
	}
	if err := (RuleMetadata{Name: r.Name, Description: r.Description}).Validate(); err != nil {
		return err
	}
	if err := r.EventType.Validate(); err != nil {
		return err
	}
	if len(r.Policy.Actions) == 0 {
		return fmt.Errorf("actions are required")
	}
	return r.Policy.Validate()
}

// ScopeType identifies an event-rule binding scope.
type ScopeType string

const (
	ScopeTypeSite ScopeType = "site"
	ScopeTypeRack ScopeType = "rack"
)

// Scope identifies either the site-wide scope or one rack.
type Scope struct {
	Type ScopeType
	ID   uuid.UUID
}

// HasID reports whether the scope carries a resource identifier.
func (s Scope) HasID() bool {
	return s.ID != uuid.Nil
}

// Validate checks the scope type and identifier contract.
func (s Scope) Validate() error {
	switch s.Type {
	case ScopeTypeSite:
		if s.HasID() {
			return fmt.Errorf("site scope must not have an id")
		}
	case ScopeTypeRack:
		if !s.HasID() {
			return fmt.Errorf("rack scope requires an id")
		}
	default:
		return fmt.Errorf("unknown event rule scope type %q", s.Type)
	}

	return nil
}

// Binding associates a persisted rule with an event-rule scope.
type Binding struct {
	ID        uuid.UUID
	RuleID    uuid.UUID
	EventType Type
	Scope     Scope
}

// Validate checks binding identity, event type, and scope.
func (b Binding) Validate() error {
	if b.ID == uuid.Nil {
		return fmt.Errorf("event rule binding id is required")
	}

	if b.RuleID == uuid.Nil {
		return fmt.Errorf("event rule binding rule id is required")
	}

	if err := b.EventType.Validate(); err != nil {
		return fmt.Errorf("event rule binding event type: %w", err)
	}

	return b.Scope.Validate()
}
