// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"fmt"
	"strings"

	"github.com/google/uuid"
)

// ScopeType identifies where a rule binding applies.
type ScopeType string

const (
	// ScopeTypeComponent binds a rule to one component.
	ScopeTypeComponent ScopeType = "component"
	// ScopeTypeRack binds a rule to one rack.
	ScopeTypeRack ScopeType = "rack"
	// ScopeTypeGlobal binds a rule to the Flow-site default scope.
	ScopeTypeGlobal ScopeType = "global"
)

// NewScopeType creates a ScopeType from a string, validating it in the
// process.
func NewScopeType(s string) (ScopeType, error) {
	st := ScopeType(strings.TrimSpace(s))
	if err := st.Validate(); err != nil {
		return "", err
	}

	return st, nil
}

// Validate checks that the scope type is supported by the first rule framework.
func (st ScopeType) Validate() error {
	switch st {
	case ScopeTypeComponent, ScopeTypeRack, ScopeTypeGlobal:
		return nil
	default:
		return fmt.Errorf("unknown scope_type %q", st)
	}
}

// String returns the string representation of the scope type.
func (st ScopeType) String() string {
	return string(st)
}

// Scope identifies where a binding applies.
type Scope struct {
	ScopeType ScopeType `json:"scope_type"`
	// ScopeID identifies the concrete resource selected by ScopeType. It must
	// be nil for ScopeTypeGlobal, a rack UUID for ScopeTypeRack, and a
	// component UUID for ScopeTypeComponent.
	ScopeID *uuid.UUID `json:"scope_id,omitempty"`
}

// Validate checks generic scope constraints.
func (s Scope) Validate() error {
	if err := s.ScopeType.Validate(); err != nil {
		return err
	}

	if s.ScopeType == ScopeTypeGlobal {
		if s.ScopeID != nil {
			return fmt.Errorf("global binding must not have a scope_id")
		}
	} else {
		if s.ScopeID == nil || *s.ScopeID == uuid.Nil {
			return fmt.Errorf("%s binding requires a scope_id", s.ScopeType)
		}
	}

	return nil
}
