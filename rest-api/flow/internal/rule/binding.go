// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"fmt"
	"time"

	"github.com/google/uuid"
)

// Binding attaches a rule to a scope with optional selector refinements.
type Binding struct {
	ID     uuid.UUID `json:"id"`
	RuleID uuid.UUID `json:"rule_id"`
	Scope
	Selector  Selector  `json:"selector,omitempty"`
	Priority  int       `json:"priority"`
	CreatedAt time.Time `json:"created_at"`
	UpdatedAt time.Time `json:"updated_at"`
}

// Validate checks generic binding constraints.
func (b *Binding) Validate() error {
	if b == nil {
		return fmt.Errorf("binding is nil")
	}

	if b.RuleID == uuid.Nil {
		return fmt.Errorf("binding rule_id is required")
	}

	if err := b.Scope.Validate(); err != nil {
		return err
	}

	if b.Priority < 0 {
		return fmt.Errorf("binding priority must be non-negative")
	}

	if err := b.Selector.Validate(); err != nil {
		return err
	}

	return nil
}
