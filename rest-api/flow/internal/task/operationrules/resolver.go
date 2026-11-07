// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"context"
	"fmt"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

// RuleStore defines the interface for operation rule persistence.
// This is a subset of the full Store interface focusing on rule operations.
type RuleStore interface {
	GetRule(ctx context.Context, id uuid.UUID) (*OperationRule, error)
	GetRuleByOperationAndRack(ctx context.Context, opType common.TaskType, operation string, rackID *uuid.UUID) (*OperationRule, error)
}

// Resolver resolves operation rules following priority hierarchy:
// 1. Database rack association (rack-specific rule via association table)
// 2. Database default rule (is_default=true for operation type + operation)
// 3. Hardcoded fallback
type Resolver struct {
	store RuleStore
}

// NewResolver creates a new rule resolver.
func NewResolver(store RuleStore) *Resolver {
	return &Resolver{
		store: store,
	}
}

// ResolveRule resolves the operation rule for a given operation type, operation, and rack.
// It always returns a rule (never nil) or an error. The resolution follows this priority:
// 0. Explicit rule ID override (caller-specified, highest priority)
// 1. Database rule (rack-specific or default)
// 2. Hardcoded default rule
func (r *Resolver) ResolveRule(
	ctx context.Context,
	operationType common.TaskType,
	operation string,
	rackID uuid.UUID,
	ruleID *uuid.UUID,
) (*OperationRule, error) {
	if r == nil {
		// If resolver is nil, return hardcoded default
		if rule := getHardcodedDefaultRule(operationType, operation); rule != nil {
			return rule, nil
		}
		return nil, fmt.Errorf("resolver is nil and no hardcoded default found for %s/%s", operationType, operation)
	}

	// Priority 0: Explicit rule ID override
	if ruleID != nil && *ruleID != uuid.Nil {
		rule, err := r.store.GetRule(ctx, *ruleID)
		if err != nil {
			return nil, fmt.Errorf("failed to fetch explicitly requested rule %s: %w", ruleID, err)
		}
		if rule == nil {
			return nil, fmt.Errorf("explicitly requested rule %s not found", ruleID)
		}
		log.Info().
			Str("rule_id", ruleID.String()).
			Str("rule_name", rule.Name).
			Str("operation_type", string(operationType)).
			Str("operation", operation).
			Str("rack_id", rackID.String()).
			Msg("Using explicitly requested operation rule")
		return rule, nil
	}

	// Priority 1: Query the database for the rule (rack association or default)
	dbRule, err := r.store.GetRuleByOperationAndRack(ctx, operationType, operation, &rackID)
	if err != nil {
		log.Warn().Err(err).
			Str("operation_type", string(operationType)).
			Str("operation", operation).
			Str("rack_id", rackID.String()).
			Msg("Failed to query database for operation rule")
	}

	if dbRule != nil {
		log.Debug().
			Str("operation_type", string(operationType)).
			Str("operation", operation).
			Str("rack_id", rackID.String()).
			Str("rule_name", dbRule.Name).
			Msg("Using database operation rule")
		return dbRule, nil
	}

	// Priority 2: Fall back to hardcoded default rule
	log.Info().
		Str("operation_type", string(operationType)).
		Str("operation", operation).
		Msg("No rule found in database, using hardcoded default")

	hardcoded := getHardcodedDefaultRule(operationType, operation)
	if hardcoded != nil {
		return hardcoded, nil
	}

	// This should never happen since hardcoded defaults cover all operations
	return nil, fmt.Errorf("no rule or hardcoded default found for %s/%s", operationType, operation)
}

// getHardcodedDefaultRule returns a pre-built hardcoded default rule for a specific operation.
// Rules are pre-built in init() in resolver_defaults.go for efficiency.
func getHardcodedDefaultRule(operationType common.TaskType, operation string) *OperationRule {
	// Look up pre-built rule from map
	key := ruleKey(operationType, operation)
	if rule := hardcodedRuleMap[key]; rule != nil {
		return rule
	}

	// No hardcoded rule found - return minimal rule for unsupported operations
	return getMinimalDefaultRule(operationType, operation)
}

// getMinimalDefaultRule returns a minimal rule for unknown operations
func getMinimalDefaultRule(operationType common.TaskType, operation string) *OperationRule {
	return &OperationRule{
		Name:          fmt.Sprintf("Minimal Default Rule for %s.%s", operationType, operation),
		Description:   "Minimal fallback rule",
		OperationType: operationType,
		OperationCode: operation,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps:   []SequenceStep{},
		},
	}
}

// GetSequenceNameForOperation maps operation-specific types to sequence names
func GetSequenceNameForOperation(operationType common.TaskType, operation any) string {
	switch operationType {
	case common.TaskTypePowerControl:
		if powerOp, ok := operation.(operations.PowerOperation); ok {
			return powerOperationToSequenceName(powerOp)
		}
	case common.TaskTypeFirmwareControl:
		if firmwareOp, ok := operation.(operations.FirmwareOperation); ok {
			return firmwareOperationToSequenceName(firmwareOp)
		}
	}
	return "default"
}

// powerOperationToSequenceName converts PowerOperation to sequence name
func powerOperationToSequenceName(op operations.PowerOperation) string {
	switch op {
	case operations.PowerOperationPowerOn:
		return SequencePowerOn
	case operations.PowerOperationForcePowerOn:
		return SequenceForcePowerOn
	case operations.PowerOperationPowerOff:
		return SequencePowerOff
	case operations.PowerOperationForcePowerOff:
		return SequenceForcePowerOff
	case operations.PowerOperationRestart:
		return SequenceRestart
	case operations.PowerOperationForceRestart:
		return SequenceForceRestart
	case operations.PowerOperationWarmReset:
		return SequenceWarmReset
	case operations.PowerOperationColdReset:
		return SequenceColdReset
	default:
		return SequencePowerOn // Default fallback
	}
}

// firmwareOperationToSequenceName converts FirmwareOperation to sequence name
func firmwareOperationToSequenceName(op operations.FirmwareOperation) string {
	switch op {
	case operations.FirmwareOperationUpgrade:
		return SequenceUpgrade
	case operations.FirmwareOperationDowngrade:
		return SequenceDowngrade
	case operations.FirmwareOperationRollback:
		return SequenceRollback
	default:
		return SequenceUpgrade // Default fallback
	}
}
