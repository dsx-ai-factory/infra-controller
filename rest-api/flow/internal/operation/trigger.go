// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operation

import (
	"fmt"

	"github.com/google/uuid"
)

// TriggerType identifies the channel that caused a task to be created.
type TriggerType string

const (
	// TriggerTypeUnspecified means generic trigger provenance was not recorded.
	TriggerTypeUnspecified TriggerType = ""
	// TriggerTypeAPI identifies a task created through the API without a durable
	// trigger occurrence.
	TriggerTypeAPI TriggerType = "api"
	// TriggerTypeEventRuleExecution identifies an event-rule action execution as
	// the task's upstream trigger.
	TriggerTypeEventRuleExecution TriggerType = "event_rule_execution"
)

// TriggerTypeFromString converts a persisted trigger type to its domain value.
func TriggerTypeFromString(value string) (TriggerType, error) {
	triggerType := TriggerType(value)
	if err := triggerType.Validate(); err != nil {
		return TriggerTypeUnspecified, err
	}

	return triggerType, nil
}

// Validate checks that the trigger type is supported by the domain.
func (t TriggerType) Validate() error {
	switch t {
	case TriggerTypeUnspecified, TriggerTypeAPI, TriggerTypeEventRuleExecution:
		return nil
	default:
		return fmt.Errorf("unknown trigger type %q", t)
	}
}

// IsSpecified reports whether the trigger type is valid and identifies a
// creation channel.
func (t TriggerType) IsSpecified() bool {
	return t != TriggerTypeUnspecified && t.Validate() == nil
}

// NeedTriggerID reports whether the trigger type requires a durable occurrence
// identity.
func (t TriggerType) NeedTriggerID() bool {
	switch t {
	case TriggerTypeEventRuleExecution:
		return true
	default:
		return false
	}
}

// ValidateTrigger checks the supported trigger type and its occurrence-ID
// contract.
func ValidateTrigger(triggerType TriggerType, triggerID *uuid.UUID) error {
	if err := triggerType.Validate(); err != nil {
		return err
	}

	if triggerType.NeedTriggerID() {
		if triggerID == nil || *triggerID == uuid.Nil {
			return fmt.Errorf("trigger type %q requires a non-zero trigger ID", triggerType)
		}

		return nil
	}

	if triggerID != nil {
		return fmt.Errorf("trigger type %q does not accept a trigger ID", triggerType)
	}

	return nil
}
