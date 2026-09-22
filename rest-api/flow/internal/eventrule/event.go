// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"encoding/json"
	"fmt"
	"time"
	"unicode/utf8"

	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/google/uuid"
)

// EventKey identifies one source event across repeated delivery attempts.
// SourceName is the stable name under which the collector type is registered;
// SourceKey is supplied or deterministically derived by that collector.
type EventKey struct {
	SourceName string
	SourceKey  string
}

// Validate checks the canonical event identity.
func (k EventKey) Validate() error {
	if err := ValidateIdentifier("event source name", k.SourceName); err != nil {
		return err
	}

	return validateRequiredString("event source key", k.SourceKey)
}

// Type identifies a domain event family.
type Type string

// Validate checks that the event type is a dotted, lowercase identifier path.
func (t Type) Validate() error {
	return validateIdentifierPath("event type", string(t))
}

// ResourceKind identifies the kind of resource an event concerns.
type ResourceKind string

const (
	ResourceKindComponent ResourceKind = "component"
	ResourceKindRack      ResourceKind = "rack"
)

// Validate checks that the resource kind is supported.
func (k ResourceKind) Validate() error {
	switch k {
	case ResourceKindComponent, ResourceKindRack:
		return nil
	default:
		return fmt.Errorf("unknown resource kind %q", k)
	}
}

// Severity identifies event or alert severity.
type Severity string

const (
	SeverityUnspecified Severity = ""
	SeverityInfo        Severity = "info"
	SeverityWarning     Severity = "warning"
	SeverityCritical    Severity = "critical"
)

// ParseSeverity converts a string into a validated Severity.
func ParseSeverity(value string) (Severity, error) {
	severity := Severity(value)
	if err := severity.Validate(); err != nil {
		return SeverityUnspecified, err
	}

	return severity, nil
}

// IsUnspecified reports whether no severity was supplied for the event.
func (s Severity) IsUnspecified() bool {
	return s == SeverityUnspecified
}

// Validate checks that severity is supported.
func (s Severity) Validate() error {
	switch s {
	case SeverityUnspecified, SeverityInfo, SeverityWarning, SeverityCritical:
		return nil
	default:
		return fmt.Errorf("unknown severity %q", s)
	}
}

// Envelope is the normalized in-memory domain type consumed by event rules.
// Transport and persistence representations convert into Envelope at their
// boundaries.
type Envelope struct {
	Key        EventKey        // Stable identity for one event across delivery retries.
	Type       Type            // Identifies the event family and payload schema.
	Severity   Severity        // Describes the event's normalized severity.
	Resource   Resource        // Identifies the resource the event concerns.
	Payload    json.RawMessage // Contains opaque event-type-specific JSON.
	ObservedAt time.Time       // Records when the source event was observed.
}

// Clone returns an independent copy of the envelope and its opaque payload.
func (e Envelope) Clone() Envelope {
	cloned := e
	cloned.Payload = append(json.RawMessage(nil), e.Payload...)

	return cloned
}

// Validate checks the normalized envelope contract.
func (e *Envelope) Validate() error {
	if e == nil {
		return fmt.Errorf("event envelope is nil")
	}

	if err := e.Key.Validate(); err != nil {
		return err
	}
	if err := e.Type.Validate(); err != nil {
		return err
	}
	if err := e.Severity.Validate(); err != nil {
		return err
	}
	if err := e.Resource.Validate(); err != nil {
		return err
	}
	if len(e.Payload) > 0 && !json.Valid(e.Payload) {
		return fmt.Errorf("event payload must be valid JSON")
	}

	return nil
}

// Resource identifies the resource an event is about.
type Resource struct {
	Kind              ResourceKind
	ExternalID        string
	ID                uuid.UUID
	ComponentTypeHint flowtypes.ComponentType
}

// Validate checks the caller-supplied resource reference.
func (r Resource) Validate() error {
	if err := r.Kind.Validate(); err != nil {
		return err
	}

	if err := validateOptionalString("resource external_id", r.ExternalID); err != nil {
		return err
	}

	if r.ComponentTypeHint != "" {
		if r.Kind != ResourceKindComponent {
			return fmt.Errorf("resource component_type_hint requires component kind")
		}

		if err := r.ComponentTypeHint.Validate(); err != nil {
			return err
		}
	}

	return nil
}

// ResolvedResource contains the canonical inventory identity and attributes
// established during event enrichment.
type ResolvedResource struct {
	Kind          ResourceKind
	ID            uuid.UUID
	RackID        uuid.UUID
	ComponentType flowtypes.ComponentType
}

// ResourceIdentity is the durable canonical identity of the resource an event
// concerns. Planning captures required topology attributes in immutable
// execution plans rather than storing them on the event.
type ResourceIdentity struct {
	Kind ResourceKind
	ID   uuid.UUID
}

// Validate checks the durable resource identity.
func (r ResourceIdentity) Validate() error {
	if err := r.Kind.Validate(); err != nil {
		return err
	}
	if r.ID == uuid.Nil {
		return fmt.Errorf("event resource id is required")
	}

	return nil
}

const maxEventSummaryRunes = 1024

// Event is one deduplicated, enriched, rule-matched observation. It owns the
// immutable information shared by all action executions.
type Event struct {
	ID              uuid.UUID
	Key             EventKey
	Type            Type
	Resource        ResourceIdentity
	AppliedRuleID   uuid.UUID
	EffectivePolicy Policy
	Summary         string
	Observations    int
	CreatedAt       time.Time
	LastObservedAt  time.Time
}

// Clone returns an independent event snapshot.
func (e Event) Clone() Event {
	cloned := e
	cloned.EffectivePolicy = e.EffectivePolicy.Clone()

	return cloned
}

// ValidateDefinition checks the immutable content supplied when creating an
// event. Persistence-owned identity, timestamps, and observation counts are
// intentionally excluded.
func (e Event) ValidateDefinition() error {
	if err := e.Key.Validate(); err != nil {
		return err
	}
	if err := e.Type.Validate(); err != nil {
		return err
	}
	if err := e.Resource.Validate(); err != nil {
		return err
	}
	if e.AppliedRuleID == uuid.Nil {
		return fmt.Errorf("applied event rule id is required")
	}
	if err := e.EffectivePolicy.Validate(); err != nil {
		return fmt.Errorf("effective policy: %w", err)
	}
	if err := validateRequiredString("event summary", e.Summary); err != nil {
		return err
	}
	if utf8.RuneCountInString(e.Summary) > maxEventSummaryRunes {
		return fmt.Errorf("event summary exceeds %d characters", maxEventSummaryRunes)
	}

	return nil
}

// Validate checks the complete durable event aggregate.
func (e *Event) Validate() error {
	if e == nil {
		return fmt.Errorf("event is nil")
	}

	if e.ID == uuid.Nil {
		return fmt.Errorf("event id is required")
	}
	if err := e.ValidateDefinition(); err != nil {
		return err
	}
	if e.Observations <= 0 {
		return fmt.Errorf("event observations must be positive")
	}
	if e.CreatedAt.IsZero() {
		return fmt.Errorf("event creation time is required")
	}
	if e.LastObservedAt.IsZero() {
		return fmt.Errorf("event last-observed time is required")
	}
	if e.LastObservedAt.Before(e.CreatedAt) {
		return fmt.Errorf("event last-observed time cannot precede creation time")
	}

	return nil
}

// NewEvent constructs a durable event using the store's authoritative time.
func NewEvent(definition Event, now time.Time) (*Event, error) {
	if err := definition.ValidateDefinition(); err != nil {
		return nil, err
	}
	if now.IsZero() {
		return nil, fmt.Errorf("event creation time is required")
	}

	event := definition.Clone()
	event.ID = uuid.New()
	event.Observations = 1
	event.CreatedAt = now
	event.LastObservedAt = now

	return &event, nil
}

// Validate checks the canonical identity and attributes established during
// enrichment.
func (r ResolvedResource) Validate() error {
	if err := r.Kind.Validate(); err != nil {
		return err
	}
	if r.ID == uuid.Nil {
		return fmt.Errorf("resolved resource id is required")
	}
	if r.RackID == uuid.Nil {
		return fmt.Errorf("resolved resource rack id is required")
	}

	if r.Kind == ResourceKindComponent {
		if err := r.ComponentType.Validate(); err != nil {
			return fmt.Errorf("resolved resource component type: %w", err)
		}
	} else {
		if r.ID != r.RackID {
			return fmt.Errorf("resolved rack resource id must equal rack id")
		}
	}

	return nil
}
