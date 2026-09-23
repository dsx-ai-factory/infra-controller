// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"encoding/json"
	"fmt"
	"sort"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/google/uuid"
)

// Current rule definition version
const CurrentRuleDefinitionVersion = "v1"

// Action names used in YAML configuration (executor-agnostic)
const (
	ActionSleep                     = "Sleep"
	ActionPowerControl              = "PowerControl"
	ActionVerifyPowerStatus         = "VerifyPowerStatus"
	ActionVerifyReachability        = "VerifyReachability"
	ActionGetPowerStatus            = "GetPowerStatus"
	ActionFirmwareControl           = "FirmwareControl"
	ActionVerifyFirmwareVersion     = "VerifyFirmwareVersion"
	ActionVerifyFirmwareConsistency = "VerifyFirmwareConsistency"

	// Bring-up specific actions
	ActionBringUpControl    = "BringUpControl"
	ActionWaitBringUp       = "WaitBringUp"
	ActionInjectExpectation = "InjectExpectation"

	// Decommission specific actions
	ActionDecommissionControl = "DecommissionControl"
	ActionWaitDecommissioned  = "WaitDecommissioned"
)

// Parameter keys for ActionConfig.Parameters
const (
	ParamDuration       = "duration"        // For Sleep (time.Duration or string)
	ParamExpectedStatus = "expected_status" // For VerifyPowerStatus (string: "on"/"off")
	ParamComponentTypes = "component_types" // For VerifyReachability ([]string)
	ParamOperation      = "operation"       // For PowerControl/FirmwareControl (optional)
	ParamPollInterval   = "poll_interval"   // For FirmwareControl: firmware update poll interval
	ParamPollTimeout    = "poll_timeout"    // For FirmwareControl: firmware update poll timeout
	ParamRequireAll     = "require_all"     // For VerifyReachability: require every component to respond
)

// RackRuleAssociation represents an association between a rack and an operation rule.
// Each association maps a specific rack to a rule for a particular operation.
type RackRuleAssociation struct {
	RackID        uuid.UUID
	OperationType common.TaskType
	OperationCode string
	RuleID        uuid.UUID
	CreatedAt     time.Time
	UpdatedAt     time.Time
}

// OperationRule represents a complete operation rule with metadata.
// Rules are templates that define how operations should be performed.
// Each rule applies to a specific operation type + operation code combination.
type OperationRule struct {
	ID             uuid.UUID       `json:"id"`
	Name           string          `json:"name"`
	Description    string          `json:"description,omitempty"`
	OperationType  common.TaskType `json:"operation_type"` // e.g., "power_control", "firmware_control"
	OperationCode  string          `json:"operation_code"` // e.g., "power_on", "power_off", "upgrade"
	RuleDefinition RuleDefinition  `json:"rule_definition"`
	IsDefault      bool            `json:"is_default"`
	CreatedAt      time.Time       `json:"created_at"`
	UpdatedAt      time.Time       `json:"updated_at"`
}

// RuleDefinition contains the actual execution steps for an operation.
// Each rule defines how to execute a specific operation (e.g., power_on, upgrade).
type RuleDefinition struct {
	Version string         `json:"version"` // Schema version for forward compatibility
	Steps   []SequenceStep `json:"steps"`
}

// HasApplicableStep reports whether at least one rule step targets one of the
// component types selected for a task. Rules describe rack-wide sequencing,
// while a task may intentionally carry only a component-scoped subset.
func (rd *RuleDefinition) HasApplicableStep(
	componentTypes map[devicetypes.ComponentType]struct{},
) bool {
	if rd == nil || len(componentTypes) == 0 {
		return false
	}

	for _, step := range rd.Steps {
		if _, ok := componentTypes[step.ComponentType]; ok {
			return true
		}
	}

	return false
}

// Stage represents a single execution stage with all its steps
type Stage struct {
	Number int            // The actual stage number from the rule definition
	Steps  []SequenceStep // All steps in this stage (execute in parallel)
}

// StageIterator provides ordered iteration over rule execution stages
// Use Next() to retrieve stages in order until it returns nil
type StageIterator struct {
	stages   []Stage
	position int
}

// NewStageIterator creates an iterator for the given rule definition
// Stages are pre-sorted by stage number, ready for sequential execution
func NewStageIterator(ruleDef *RuleDefinition) *StageIterator {
	if ruleDef == nil || len(ruleDef.Steps) == 0 {
		return &StageIterator{stages: nil, position: 0}
	}

	// Group steps by stage number
	stageMap := make(map[int][]SequenceStep)
	for _, step := range ruleDef.Steps {
		stageMap[step.Stage] = append(stageMap[step.Stage], step)
	}

	// Extract and sort stage numbers
	stageNums := make([]int, 0, len(stageMap))
	for num := range stageMap {
		stageNums = append(stageNums, num)
	}
	sort.Ints(stageNums)

	// Build ordered stages slice
	stages := make([]Stage, 0, len(stageNums))
	for _, num := range stageNums {
		stages = append(stages, Stage{
			Number: num,
			Steps:  stageMap[num],
		})
	}

	return &StageIterator{
		stages:   stages,
		position: 0,
	}
}

// Next returns the next stage in execution order
// Returns nil when all stages have been returned
func (si *StageIterator) Next() *Stage {
	if si == nil || si.position >= len(si.stages) {
		return nil
	}

	stage := &si.stages[si.position]
	si.position++
	return stage
}

// HasNext returns true if there are more stages to iterate
// Useful for checking without advancing the iterator
func (si *StageIterator) HasNext() bool {
	return si != nil && si.position < len(si.stages)
}

// Total returns the number of stages in the rule definition.
func (si *StageIterator) Total() int {
	if si == nil {
		return 0
	}
	return len(si.stages)
}

// Reset resets the iterator to the beginning
// Allows re-iteration without creating a new iterator
func (si *StageIterator) Reset() {
	if si != nil {
		si.position = 0
	}
}

// ActionConfig defines configuration for a single action execution
// Note: "Action" is executor-agnostic (works with Temporal, future executors)
type ActionConfig struct {
	Name         string         `json:"name"`                    // Action name
	Timeout      time.Duration  `json:"timeout,omitempty"`       // Optional override
	PollInterval time.Duration  `json:"poll_interval,omitempty"` // For polling actions
	Parameters   map[string]any `json:"parameters,omitempty"`    // Action-specific params
}

// SequenceStep defines a single step in the execution sequence
type SequenceStep struct {
	// Which component type this step applies to
	ComponentType devicetypes.ComponentType `json:"component_type"`

	// Execution stage number (steps with same stage run in parallel)
	// Stage 1 executes first, then stage 2, etc.
	Stage int `json:"stage"`

	// Maximum parallel component operations (0 = unlimited, 1 = sequential)
	// Controls how many components of this type are processed concurrently
	MaxParallel int `json:"max_parallel"`

	// Child workflow configuration (applies to entire workflow: pre + main + post)
	Timeout     time.Duration `json:"timeout,omitempty"` // Child workflow timeout
	RetryPolicy *RetryPolicy  `json:"retry,omitempty"`   // Child workflow retry

	// Action sequences
	PreOperation  []ActionConfig `json:"pre_operation,omitempty"`  // Before main operation
	MainOperation ActionConfig   `json:"main_operation"`           // Primary operation
	PostOperation []ActionConfig `json:"post_operation,omitempty"` // After main operation

	// Legacy field for backward compatibility (deprecated: use MainOperation)
	DelayAfter time.Duration `json:"delay_after,omitempty"`
}

// RetryPolicy defines retry behavior for a step
type RetryPolicy struct {
	MaxAttempts        int           `json:"max_attempts"`
	InitialInterval    time.Duration `json:"initial_interval"`       // Parsed once at rule creation time
	BackoffCoefficient float64       `json:"backoff_coefficient"`    // Exponential backoff multiplier
	MaxInterval        time.Duration `json:"max_interval,omitempty"` // Parsed once at rule creation time
}

// MarshalJSON customizes JSON output for ActionConfig
func (ac ActionConfig) MarshalJSON() ([]byte, error) {
	type Alias ActionConfig
	return json.Marshal(&struct {
		Timeout      string `json:"timeout,omitempty"`
		PollInterval string `json:"poll_interval,omitempty"`
		*Alias
	}{
		Timeout:      ac.Timeout.String(),
		PollInterval: ac.PollInterval.String(),
		Alias:        (*Alias)(&ac),
	})
}

// UnmarshalJSON customizes JSON parsing for ActionConfig
func (ac *ActionConfig) UnmarshalJSON(data []byte) error {
	type Alias ActionConfig
	aux := &struct {
		Timeout      string `json:"timeout,omitempty"`
		PollInterval string `json:"poll_interval,omitempty"`
		*Alias
	}{
		Alias: (*Alias)(ac),
	}

	if err := json.Unmarshal(data, aux); err != nil {
		return err
	}

	// Parse timeout
	if d, err := ParseDuration(aux.Timeout, "timeout"); err != nil {
		return err
	} else {
		ac.Timeout = d
	}

	// Parse poll interval
	if d, err := ParseDuration(aux.PollInterval, "poll_interval"); err != nil {
		return err
	} else {
		ac.PollInterval = d
	}

	return nil
}

// MarshalJSON customizes JSON output for SequenceStep to format durations as strings
func (s SequenceStep) MarshalJSON() ([]byte, error) {
	type Alias SequenceStep
	return json.Marshal(&struct {
		DelayAfter string `json:"delay_after,omitempty"`
		Timeout    string `json:"timeout,omitempty"`
		*Alias
	}{
		DelayAfter: s.DelayAfter.String(),
		Timeout:    s.Timeout.String(),
		Alias:      (*Alias)(&s),
	})
}

// UnmarshalJSON customizes JSON parsing for SequenceStep to parse duration strings
func (s *SequenceStep) UnmarshalJSON(data []byte) error {
	type Alias SequenceStep
	aux := &struct {
		DelayAfter string `json:"delay_after,omitempty"`
		Timeout    string `json:"timeout,omitempty"`
		*Alias
	}{
		Alias: (*Alias)(s),
	}

	if err := json.Unmarshal(data, aux); err != nil {
		return err
	}

	// Parse duration fields
	d, err := ParseDuration(aux.DelayAfter, "delay_after")
	if err != nil {
		return err
	}
	s.DelayAfter = d

	d, err = ParseDuration(aux.Timeout, "timeout")
	if err != nil {
		return err
	}
	s.Timeout = d

	return nil
}

// MarshalJSON customizes JSON output for RetryPolicy to format durations as strings
func (rp RetryPolicy) MarshalJSON() ([]byte, error) {
	type Alias RetryPolicy
	return json.Marshal(&struct {
		InitialInterval string `json:"initial_interval"`
		MaxInterval     string `json:"max_interval,omitempty"`
		*Alias
	}{
		InitialInterval: rp.InitialInterval.String(),
		MaxInterval:     rp.MaxInterval.String(),
		Alias:           (*Alias)(&rp),
	})
}

// UnmarshalJSON customizes JSON parsing for RetryPolicy to parse duration strings
func (rp *RetryPolicy) UnmarshalJSON(data []byte) error {
	type Alias RetryPolicy
	aux := &struct {
		InitialInterval string `json:"initial_interval"`
		MaxInterval     string `json:"max_interval,omitempty"`
		*Alias
	}{
		Alias: (*Alias)(rp),
	}

	if err := json.Unmarshal(data, aux); err != nil {
		return err
	}

	// Parse initial interval (required)
	d, err := ParseDuration(aux.InitialInterval, "initial_interval")
	if err != nil {
		return err
	}
	rp.InitialInterval = d

	// Parse max interval (optional)
	d, err = ParseDuration(aux.MaxInterval, "max_interval")
	if err != nil {
		return err
	}
	rp.MaxInterval = d

	return nil
}

// ParseDuration safely parses a duration string
func ParseDuration(s string, name string) (time.Duration, error) {
	if s == "" {
		return 0, nil
	}

	d, err := time.ParseDuration(s)
	if err != nil {
		return 0, fmt.Errorf("invalid %s ('%s'): %w", name, s, err)
	}

	return d, nil
}

// MarshalRuleDefinition converts RuleDefinition to JSON for database storage
// MarshalRuleDefinition converts RuleDefinition to JSON, ensuring version is set
func MarshalRuleDefinition(rd RuleDefinition) (json.RawMessage, error) {
	// Ensure version is set to current version
	if rd.Version == "" {
		rd.Version = CurrentRuleDefinitionVersion
	}
	return json.Marshal(rd)
}

// UnmarshalRuleDefinition converts JSON to RuleDefinition with version awareness
func UnmarshalRuleDefinition(data json.RawMessage) (*RuleDefinition, error) {
	// First, peek at the version field
	var versionCheck struct {
		Version string `json:"version"`
	}
	if err := json.Unmarshal(data, &versionCheck); err != nil {
		return nil, fmt.Errorf("failed to read version: %w", err)
	}

	// Unmarshal based on version
	switch versionCheck.Version {
	case "":
		// Handle missing version (assume v1 for backward compatibility)
		return unmarshalRuleDefinitionV1(data)
	case "v1":
		return unmarshalRuleDefinitionV1(data)
	default:
		return nil, fmt.Errorf(
			"unsupported rule definition version: %s (current version: %s)",
			versionCheck.Version,
			CurrentRuleDefinitionVersion,
		)
	}
}

// unmarshalRuleDefinitionV1 handles v1 format unmarshaling
func unmarshalRuleDefinitionV1(data json.RawMessage) (*RuleDefinition, error) {
	var rd RuleDefinition
	if err := json.Unmarshal(data, &rd); err != nil {
		return nil, fmt.Errorf("failed to unmarshal v1 rule definition: %w", err)
	}

	// Set version if not present (for rules created before versioning)
	if rd.Version == "" {
		rd.Version = "v1"
	}

	return &rd, nil
}

// Validate validates a complete operation rule
func (rule *OperationRule) Validate() error {
	if rule == nil {
		return fmt.Errorf("rule is nil")
	}

	if rule.Name == "" {
		return fmt.Errorf("rule name is required")
	}

	if !rule.OperationType.IsValid() {
		return fmt.Errorf("invalid operation type: %s", rule.OperationType)
	}

	if !IsValidOperation(rule.OperationType, rule.OperationCode) {
		return fmt.Errorf(
			"invalid operation code '%s' for operation type '%s'",
			rule.OperationCode,
			rule.OperationType,
		)
	}

	return rule.RuleDefinition.Validate()
}

// SuggestOptimizations analyzes a rule and suggests potential optimizations
// This is a helper method for users to improve their rules
func (rule *OperationRule) SuggestOptimizations() []string {
	var suggestions []string

	// Check for sequential stages that could be parallelized
	iter := NewStageIterator(&rule.RuleDefinition)
	for stage := iter.Next(); stage != nil; stage = iter.Next() {
		if len(stage.Steps) == 1 && stage.Steps[0].MaxParallel == 1 {
			suggestions = append(suggestions,
				fmt.Sprintf("Stage %d: Consider increasing max_parallel from 1 for faster execution", stage.Number))
		}
	}

	// Check for overly conservative max_parallel settings
	for _, step := range rule.RuleDefinition.Steps {
		if step.MaxParallel == 1 {
			suggestions = append(suggestions,
				fmt.Sprintf("%s (stage %d): max_parallel=1 is very conservative, consider batching",
					devicetypes.ComponentTypeToString(step.ComponentType), step.Stage))
		}
	}

	// Check for excessive delays
	for _, step := range rule.RuleDefinition.Steps {
		if step.DelayAfter > 60*time.Second {
			suggestions = append(suggestions,
				fmt.Sprintf("%s (stage %d): delay_after of %v may be excessive",
					devicetypes.ComponentTypeToString(step.ComponentType), step.Stage, step.DelayAfter))
		}
	}

	return suggestions
}

// Validate validates a rule definition
func (rd *RuleDefinition) Validate() error {
	if rd == nil {
		return fmt.Errorf("rule definition is nil")
	}

	// Steps can be empty for operations with hardcoded
	// sequencing (e.g., bring-up, firmware update).
	if len(rd.Steps) == 0 {
		return nil
	}

	// Track component types to detect duplicates within same stage
	stageComponentTypes := make(map[int]map[devicetypes.ComponentType]bool)

	for i, step := range rd.Steps {
		if err := step.Validate(); err != nil {
			return fmt.Errorf("step %d: %w", i, err)
		}

		// Check for duplicate component types in same stage
		if stageComponentTypes[step.Stage] == nil {
			stageComponentTypes[step.Stage] = make(map[devicetypes.ComponentType]bool)
		}
		if stageComponentTypes[step.Stage][step.ComponentType] {
			return fmt.Errorf("step %d: duplicate component type %s in stage %d",
				i, devicetypes.ComponentTypeToString(step.ComponentType), step.Stage)
		}
		stageComponentTypes[step.Stage][step.ComponentType] = true
	}

	return nil
}

// GroupStepsByStage organizes steps by their stage number
// Returns a map where keys are stage numbers and values are slices of steps
func (rd *RuleDefinition) GroupStepsByStage() map[int][]SequenceStep {
	stages := make(map[int][]SequenceStep)
	for _, step := range rd.Steps {
		stages[step.Stage] = append(stages[step.Stage], step)
	}
	return stages
}

// GetMaxStage returns the highest stage number in the rule
func (rd *RuleDefinition) GetMaxStage() int {
	maxStage := 0
	for _, step := range rd.Steps {
		if step.Stage > maxStage {
			maxStage = step.Stage
		}
	}
	return maxStage
}

// CalculateWorkflowTimeout computes total workflow timeout from steps.
// Parent workflow timeout is auto-calculated: stages run sequentially,
// steps within a stage run in parallel (take max timeout).
// Adds 10% safety buffer for orchestration overhead.
func (rd *RuleDefinition) CalculateWorkflowTimeout() time.Duration {
	if rd == nil || len(rd.Steps) == 0 {
		return 0
	}

	// Group steps by stage
	stageMap := make(map[int][]SequenceStep)
	for _, step := range rd.Steps {
		stageMap[step.Stage] = append(stageMap[step.Stage], step)
	}

	totalTimeout := time.Duration(0)

	// For each stage: stage timeout = max child workflow timeout
	// (children run in parallel)
	for _, stageSteps := range stageMap {
		maxStepTimeout := time.Duration(0)
		for _, step := range stageSteps {
			if step.Timeout > maxStepTimeout {
				maxStepTimeout = step.Timeout
			}
		}
		totalTimeout += maxStepTimeout // Stages run sequentially
	}

	// Add 10% safety buffer for orchestration overhead
	buffer := totalTimeout / 10
	return totalTimeout + buffer
}

// Validate validates a single sequence step
// The index parameter is used for error messages
func (step *SequenceStep) Validate() error {
	// Validate component type
	if step.ComponentType == devicetypes.ComponentTypeUnknown {
		return fmt.Errorf("component type is unknown or invalid")
	}

	// Validate stage
	if step.Stage < 1 {
		return fmt.Errorf("stage must be >= 1, got %d", step.Stage)
	}

	// Validate max_parallel
	if step.MaxParallel < 0 {
		return fmt.Errorf("max_parallel must be >= 0, got %d", step.MaxParallel)
	}

	// Validate delay_after duration (legacy field)
	if step.DelayAfter < 0 {
		return fmt.Errorf("delay_after must be non-negative, got %v", step.DelayAfter)
	}

	// Validate timeout duration
	if step.Timeout < 0 {
		return fmt.Errorf("timeout must be non-negative, got %v", step.Timeout)
	}

	// Validate retry policy if present
	if step.RetryPolicy != nil {
		if err := step.RetryPolicy.Validate(); err != nil {
			return fmt.Errorf("invalid retry policy: %w", err)
		}
	}

	// Validate pre-operation actions
	for i, action := range step.PreOperation {
		if err := action.Validate(); err != nil {
			return fmt.Errorf("pre_operation[%d]: %w", i, err)
		}
	}

	if step.MainOperation.Name == "" {
		return fmt.Errorf("main_operation is required")
	}

	if err := step.MainOperation.Validate(); err != nil {
		return fmt.Errorf("main_operation: %w", err)
	}

	// Validate post-operation actions
	for i, action := range step.PostOperation {
		if err := action.Validate(); err != nil {
			return fmt.Errorf("post_operation[%d]: %w", i, err)
		}
	}

	return nil
}

// DoPreOperations returns whether there are pre-operation actions to execute
// and the list of actions. This encapsulates the check and data access.
func (step *SequenceStep) DoPreOperations() (bool, []ActionConfig) {
	if len(step.PreOperation) == 0 {
		return false, nil
	}
	return true, step.PreOperation
}

// DoMainOperation returns whether there is a main operation to execute
// and the action configuration.
func (step *SequenceStep) DoMainOperation() (bool, ActionConfig) {
	if step.MainOperation.Name == "" {
		return false, ActionConfig{}
	}
	return true, step.MainOperation
}

// DoPostOperations returns whether there are post-operation actions to execute
// and the list of actions. This encapsulates the check and data access.
func (step *SequenceStep) DoPostOperations() (bool, []ActionConfig) {
	if len(step.PostOperation) == 0 {
		return false, nil
	}
	return true, step.PostOperation
}

// OrderedActions returns all actions in the order a sequence step executes
// them: pre-operation actions, main operation, then post-operation actions.
func (step SequenceStep) OrderedActions() []ActionConfig {
	actions := make([]ActionConfig, 0,
		len(step.PreOperation)+1+len(step.PostOperation))
	actions = append(actions, step.PreOperation...)
	if step.MainOperation.Name != "" {
		actions = append(actions, step.MainOperation)
	}
	actions = append(actions, step.PostOperation...)
	return actions
}

// Validate validates a retry policy
func (rp *RetryPolicy) Validate() error {
	if rp.MaxAttempts < 1 {
		return fmt.Errorf("max_attempts must be >= 1, got %d", rp.MaxAttempts)
	}

	if rp.InitialInterval <= 0 {
		return fmt.Errorf("initial_interval is required and must be positive")
	}

	if rp.BackoffCoefficient < 1.0 {
		return fmt.Errorf("backoff_coefficient must be >= 1.0, got %f", rp.BackoffCoefficient)
	}

	if rp.MaxInterval < 0 {
		return fmt.Errorf("max_interval must be non-negative, got %v", rp.MaxInterval)
	}

	return nil
}
