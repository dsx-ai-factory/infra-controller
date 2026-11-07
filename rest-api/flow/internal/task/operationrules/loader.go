// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"fmt"
	"os"

	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type RuleLoader interface {
	Load() (map[common.TaskType]map[string]*OperationRule, error)
}

type YAMLRuleLoader struct {
	path string
}

func NewYAMLRuleLoader(path string) (RuleLoader, error) {
	if _, err := os.Stat(path); os.IsNotExist(err) {
		return nil, fmt.Errorf("YAML file not found: %s", path)
	}

	return &YAMLRuleLoader{path: path}, nil
}

func (l *YAMLRuleLoader) Load() (map[common.TaskType]map[string]*OperationRule, error) {
	data, err := os.ReadFile(l.path)
	if err != nil {
		return nil, fmt.Errorf("failed to read YAML file: %w", err)
	}

	var config YAMLConfig
	if err := yaml.Unmarshal(data, &config); err != nil {
		return nil, fmt.Errorf("failed to parse YAML: %w", err)
	}

	// Organize rules by operation type and operation name
	rules := make(map[common.TaskType]map[string]*OperationRule)

	for i, yamlRule := range config.Rules {
		// Convert YAML steps
		steps, err := convertYAMLSteps(yamlRule.Steps)
		if err != nil {
			return nil, fmt.Errorf(
				"rule %d (%s) from YAML file %s: %w",
				i,
				yamlRule.Name,
				l.path,
				err,
			)
		}

		// Create rule
		rule := &OperationRule{
			Name:          yamlRule.Name,
			Description:   yamlRule.Description,
			OperationType: common.TaskTypeFromString(yamlRule.OperationType),
			OperationCode: yamlRule.Operation,
			RuleDefinition: RuleDefinition{
				Version: CurrentRuleDefinitionVersion,
				Steps:   steps,
			},
			IsDefault: true, // YAML rules are default rules
		}

		// Validate the rule loaded from YAML file. NOTE: we can validate
		// the steps loaded from YAML file earlier to detect errors early,
		// but it's more efficient to validate the rule as a whole.
		if err := rule.Validate(); err != nil {
			return nil, fmt.Errorf(
				"invalid rule %d from YAML file %s: %w",
				i,
				l.path,
				err,
			)
		}

		// Add to map
		if rules[rule.OperationType] == nil {
			rules[rule.OperationType] = make(map[string]*OperationRule)
		}

		if rules[rule.OperationType][rule.OperationCode] != nil {
			return nil, fmt.Errorf(
				"duplicate rule %s for operation type %s",
				rule.Name,
				rule.OperationType,
			)
		}

		rules[rule.OperationType][rule.OperationCode] = rule
	}

	// Validate that all required operations have rules
	for opType, opRules := range rules {
		if err := ValidateRequiredOperations(opType, opRules); err != nil {
			return nil, fmt.Errorf("validation failed for %s: %w", opType, err)
		}
	}

	return rules, nil
}

// YAMLConfig represents the top-level structure of the operation rules YAML file
type YAMLConfig struct {
	Version string     `yaml:"version"`
	Rules   []YAMLRule `yaml:"rules"`
}

// YAMLRule represents a single operation rule in the YAML config
type YAMLRule struct {
	Name          string     `yaml:"name"`
	Description   string     `yaml:"description,omitempty"`
	OperationType string     `yaml:"operation_type"`
	Operation     string     `yaml:"operation"`
	Steps         []YAMLStep `yaml:"steps"`
}

// YAMLStep represents a single step in the YAML config
type YAMLStep struct {
	ComponentType string             `yaml:"component_type"`
	Stage         int                `yaml:"stage"`
	MaxParallel   int                `yaml:"max_parallel"`
	Timeout       string             `yaml:"timeout,omitempty"`
	Retry         *YAMLRetryPolicy   `yaml:"retry,omitempty"`
	PreOperation  []YAMLActionConfig `yaml:"pre_operation,omitempty"`
	MainOperation YAMLActionConfig   `yaml:"main_operation"`
	PostOperation []YAMLActionConfig `yaml:"post_operation,omitempty"`
	DelayAfter    string             `yaml:"delay_after,omitempty"` // Legacy
}

// YAMLActionConfig represents an action configuration in YAML
type YAMLActionConfig struct {
	Name         string         `yaml:"name"`
	Timeout      string         `yaml:"timeout,omitempty"`
	PollInterval string         `yaml:"poll_interval,omitempty"`
	Parameters   map[string]any `yaml:"parameters,omitempty"`
}

// YAMLRetryPolicy represents retry configuration in YAML
type YAMLRetryPolicy struct {
	MaxAttempts        int     `yaml:"max_attempts"`
	InitialInterval    string  `yaml:"initial_interval"`
	BackoffCoefficient float64 `yaml:"backoff_coefficient"`
	MaxInterval        string  `yaml:"max_interval,omitempty"`
}

// toRetryPolicy converts YAML retry policy to RetryPolicy
func (yr *YAMLRetryPolicy) toRetryPolicy() (*RetryPolicy, error) {
	retryPolicy := &RetryPolicy{
		MaxAttempts:        yr.MaxAttempts,
		BackoffCoefficient: yr.BackoffCoefficient,
	}

	// Parse initial interval (required)
	d, err := ParseDuration(yr.InitialInterval, "initial_interval")
	if err != nil {
		return nil, err
	}
	retryPolicy.InitialInterval = d

	// Parse max interval (optional)
	d, err = ParseDuration(yr.MaxInterval, "max_interval")
	if err != nil {
		return nil, err
	}
	retryPolicy.MaxInterval = d

	return retryPolicy, nil
}

// toActionConfig converts YAML action config to ActionConfig
func (ya *YAMLActionConfig) toActionConfig() (ActionConfig, error) {
	action := ActionConfig{
		Name:       ya.Name,
		Parameters: ya.Parameters,
	}

	// Parse timeout
	d, err := ParseDuration(ya.Timeout, "timeout")
	if err != nil {
		return ActionConfig{}, err
	}
	action.Timeout = d

	// Parse poll interval
	d, err = ParseDuration(ya.PollInterval, "poll_interval")
	if err != nil {
		return ActionConfig{}, err
	}
	action.PollInterval = d

	// Parse duration parameter if present (for Sleep action)
	if params := action.Parameters; params != nil {
		if durationVal, ok := params[ParamDuration]; ok {
			if durationStr, ok := durationVal.(string); ok {
				d, err := ParseDuration(
					durationStr,
					fmt.Sprintf("duration parameter for action '%s'", ya.Name),
				)
				if err != nil {
					return ActionConfig{}, err
				}
				params[ParamDuration] = d
			}
		}
	}

	return action, nil
}

// toSequenceStep converts YAML step to SequenceStep
func (ys *YAMLStep) toSequenceStep() (SequenceStep, error) {
	// Convert component type string to enum
	componentType := devicetypes.ComponentTypeFromString(ys.ComponentType)
	step := SequenceStep{
		ComponentType: componentType,
		Stage:         ys.Stage,
		MaxParallel:   ys.MaxParallel,
	}

	// Parse timeout
	d, err := ParseDuration(ys.Timeout, "timeout")
	if err != nil {
		return SequenceStep{}, err
	}
	step.Timeout = d

	// Convert retry policy if present
	if ys.Retry != nil {
		retryPolicy, err := ys.Retry.toRetryPolicy()
		if err != nil {
			return SequenceStep{}, fmt.Errorf("retry: %w", err)
		}
		step.RetryPolicy = retryPolicy
	}

	// Convert action configs
	preOps, err := toActionConfigs(ys.PreOperation)
	if err != nil {
		return SequenceStep{}, fmt.Errorf("pre_operation: %w", err)
	}
	step.PreOperation = preOps

	mainOp, err := ys.MainOperation.toActionConfig()
	if err != nil {
		return SequenceStep{}, fmt.Errorf("main_operation: %w", err)
	}
	step.MainOperation = mainOp

	postOps, err := toActionConfigs(ys.PostOperation)
	if err != nil {
		return SequenceStep{}, fmt.Errorf("post_operation: %w", err)
	}
	step.PostOperation = postOps

	// Legacy field support
	d, err = ParseDuration(ys.DelayAfter, "delay_after")
	if err != nil {
		return SequenceStep{}, err
	}
	step.DelayAfter = d

	return step, nil
}

// toActionConfigs converts slice of YAML action configs to ActionConfigs
func toActionConfigs(yamlActions []YAMLActionConfig) ([]ActionConfig, error) {
	if len(yamlActions) == 0 {
		return nil, nil
	}

	actions := make([]ActionConfig, 0, len(yamlActions))
	for i, ya := range yamlActions {
		action, err := ya.toActionConfig()
		if err != nil {
			return nil, fmt.Errorf("action[%d]: %w", i, err)
		}
		actions = append(actions, action)
	}
	return actions, nil
}

// convertYAMLSteps converts YAML steps to SequenceStep slice
func convertYAMLSteps(yamlSteps []YAMLStep) ([]SequenceStep, error) {
	steps := make([]SequenceStep, 0, len(yamlSteps))

	for i, yamlStep := range yamlSteps {
		step, err := yamlStep.toSequenceStep()
		if err != nil {
			componentType := devicetypes.ComponentTypeFromString(
				yamlStep.ComponentType,
			)
			return nil, fmt.Errorf("step[%d] (%s): %w", i, componentType, err)
		}
		steps = append(steps, step)
	}

	return steps, nil
}
