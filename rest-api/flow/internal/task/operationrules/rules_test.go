// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestNewStageIterator(t *testing.T) {
	t.Run("sequential stages", func(t *testing.T) {
		ruleDef := &RuleDefinition{
			Steps: []SequenceStep{
				{ComponentType: devicetypes.ComponentTypeCompute, Stage: 1, MaxParallel: 1},
				{ComponentType: devicetypes.ComponentTypeNVSwitch, Stage: 2, MaxParallel: 2},
				{ComponentType: devicetypes.ComponentTypePowerShelf, Stage: 3, MaxParallel: 1},
			},
		}

		iter := NewStageIterator(ruleDef)

		stage1 := iter.Next()
		require.NotNil(t, stage1, "expected first stage")
		assert.Equal(t, 1, stage1.Number)
		assert.Len(t, stage1.Steps, 1)
		assert.Equal(t, devicetypes.ComponentTypeCompute, stage1.Steps[0].ComponentType)

		stage2 := iter.Next()
		require.NotNil(t, stage2, "expected second stage")
		assert.Equal(t, 2, stage2.Number)
		assert.Len(t, stage2.Steps, 1)
		assert.Equal(t, devicetypes.ComponentTypeNVSwitch, stage2.Steps[0].ComponentType)

		stage3 := iter.Next()
		require.NotNil(t, stage3, "expected third stage")
		assert.Equal(t, 3, stage3.Number)
		assert.Len(t, stage3.Steps, 1)
		assert.Equal(t, devicetypes.ComponentTypePowerShelf, stage3.Steps[0].ComponentType)

		assert.Nil(t, iter.Next(), "expected nil after all stages")
	})

	t.Run("stages with gaps", func(t *testing.T) {
		ruleDef := &RuleDefinition{
			Steps: []SequenceStep{
				{ComponentType: devicetypes.ComponentTypeCompute, Stage: 1, MaxParallel: 1},
				{ComponentType: devicetypes.ComponentTypeNVSwitch, Stage: 5, MaxParallel: 2},
			},
		}

		iter := NewStageIterator(ruleDef)

		stage1 := iter.Next()
		require.NotNil(t, stage1)
		assert.Equal(t, 1, stage1.Number)
		assert.Len(t, stage1.Steps, 1)

		stage2 := iter.Next()
		require.NotNil(t, stage2)
		assert.Equal(t, 5, stage2.Number)
		assert.Len(t, stage2.Steps, 1)

		assert.Nil(t, iter.Next())
	})

	t.Run("multiple steps in same stage", func(t *testing.T) {
		ruleDef := &RuleDefinition{
			Steps: []SequenceStep{
				{ComponentType: devicetypes.ComponentTypeCompute, Stage: 1, MaxParallel: 1},
				{ComponentType: devicetypes.ComponentTypeNVSwitch, Stage: 1, MaxParallel: 2},
			},
		}

		iter := NewStageIterator(ruleDef)
		stage := iter.Next()

		require.NotNil(t, stage)
		assert.Equal(t, 1, stage.Number)
		assert.Len(t, stage.Steps, 2)
		assert.Nil(t, iter.Next())
	})

	t.Run("empty rule definition", func(t *testing.T) {
		iter := NewStageIterator(&RuleDefinition{Steps: []SequenceStep{}})
		assert.Nil(t, iter.Next())
	})

	t.Run("nil rule definition", func(t *testing.T) {
		iter := NewStageIterator(nil)
		assert.Nil(t, iter.Next())
	})
}

func TestStageIterator_HasNext(t *testing.T) {
	ruleDef := &RuleDefinition{
		Steps: []SequenceStep{
			{ComponentType: devicetypes.ComponentTypeCompute, Stage: 1, MaxParallel: 1},
			{ComponentType: devicetypes.ComponentTypeNVSwitch, Stage: 2, MaxParallel: 2},
		},
	}

	iter := NewStageIterator(ruleDef)

	assert.True(t, iter.HasNext(), "expected HasNext=true at start")
	iter.Next()
	assert.True(t, iter.HasNext(), "expected HasNext=true after first Next()")
	iter.Next()
	assert.False(t, iter.HasNext(), "expected HasNext=false after all stages consumed")
}

func TestStageIterator_Reset(t *testing.T) {
	ruleDef := &RuleDefinition{
		Steps: []SequenceStep{
			{ComponentType: devicetypes.ComponentTypeCompute, Stage: 1, MaxParallel: 1},
			{ComponentType: devicetypes.ComponentTypeNVSwitch, Stage: 2, MaxParallel: 2},
		},
	}

	iter := NewStageIterator(ruleDef)

	iter.Next()
	iter.Next()
	assert.Nil(t, iter.Next(), "expected nil after consuming all stages")

	iter.Reset()
	assert.NotNil(t, iter.Next(), "expected first stage after reset")
	assert.NotNil(t, iter.Next(), "expected second stage after reset")
	assert.Nil(t, iter.Next(), "expected nil after second full iteration")
}

func TestStageIterator_StandardLoop(t *testing.T) {
	ruleDef := &RuleDefinition{
		Steps: []SequenceStep{
			{ComponentType: devicetypes.ComponentTypeCompute, Stage: 1, MaxParallel: 1},
			{ComponentType: devicetypes.ComponentTypeNVSwitch, Stage: 2, MaxParallel: 2},
			{ComponentType: devicetypes.ComponentTypePowerShelf, Stage: 3, MaxParallel: 1},
		},
	}

	iter := NewStageIterator(ruleDef)
	count := 0
	for stage := iter.Next(); stage != nil; stage = iter.Next() {
		count++
		assert.NotEmpty(t, stage.Steps)
	}
	assert.Equal(t, 3, count)
}

func TestSequenceStep_MarshalJSON(t *testing.T) {
	t.Run("with all duration fields", func(t *testing.T) {
		step := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         1,
			MaxParallel:   2,
			DelayAfter:    30 * time.Second,
			Timeout:       10 * time.Minute,
			RetryPolicy: &RetryPolicy{
				MaxAttempts:        3,
				InitialInterval:    5 * time.Second,
				BackoffCoefficient: 2.0,
				MaxInterval:        1 * time.Minute,
			},
		}

		data, err := json.Marshal(step)
		require.NoError(t, err)

		var result map[string]any
		require.NoError(t, json.Unmarshal(data, &result))

		assert.Equal(t, "30s", result["delay_after"])
		assert.Equal(t, "10m0s", result["timeout"])
	})

	t.Run("with zero durations", func(t *testing.T) {
		step := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         1,
			MaxParallel:   2,
		}

		data, err := json.Marshal(step)
		require.NoError(t, err)

		var result map[string]any
		require.NoError(t, json.Unmarshal(data, &result))

		assert.Equal(t, "0s", result["delay_after"])
		assert.Equal(t, "0s", result["timeout"])
	})
}

func TestSequenceStep_UnmarshalJSON(t *testing.T) {
	t.Run("valid duration strings", func(t *testing.T) {
		original := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         1,
			MaxParallel:   2,
			DelayAfter:    30 * time.Second,
			Timeout:       10 * time.Minute,
		}

		jsonData, err := json.Marshal(original)
		require.NoError(t, err)

		var step SequenceStep
		require.NoError(t, json.Unmarshal(jsonData, &step))

		assert.Equal(t, 30*time.Second, step.DelayAfter)
		assert.Equal(t, 10*time.Minute, step.Timeout)
		assert.Equal(t, devicetypes.ComponentTypeCompute, step.ComponentType)
		assert.Equal(t, 1, step.Stage)
		assert.Equal(t, 2, step.MaxParallel)
	})

	t.Run("missing duration fields", func(t *testing.T) {
		original := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         1,
			MaxParallel:   2,
		}

		jsonData, err := json.Marshal(original)
		require.NoError(t, err)

		var step SequenceStep
		require.NoError(t, json.Unmarshal(jsonData, &step))

		assert.Equal(t, time.Duration(0), step.DelayAfter)
		assert.Equal(t, time.Duration(0), step.Timeout)
	})

	t.Run("invalid delay_after format", func(t *testing.T) {
		jsonData := []byte(`{"component_type":1,"stage":1,"max_parallel":2,"delay_after":"invalid"}`)
		var step SequenceStep
		assert.Error(t, json.Unmarshal(jsonData, &step))
	})

	t.Run("invalid timeout format", func(t *testing.T) {
		jsonData := []byte(`{"component_type":1,"stage":1,"max_parallel":2,"timeout":"not-a-duration"}`)
		var step SequenceStep
		assert.Error(t, json.Unmarshal(jsonData, &step))
	})
}

func TestSequenceStep_MarshalUnmarshal_RoundTrip(t *testing.T) {
	original := SequenceStep{
		ComponentType: devicetypes.ComponentTypeNVSwitch,
		Stage:         2,
		MaxParallel:   5,
		DelayAfter:    15 * time.Second,
		Timeout:       20 * time.Minute,
		RetryPolicy: &RetryPolicy{
			MaxAttempts:        3,
			InitialInterval:    5 * time.Second,
			BackoffCoefficient: 2.0,
			MaxInterval:        1 * time.Minute,
		},
	}

	data, err := json.Marshal(original)
	require.NoError(t, err)

	var decoded SequenceStep
	require.NoError(t, json.Unmarshal(data, &decoded))

	assert.Equal(t, original.ComponentType, decoded.ComponentType)
	assert.Equal(t, original.Stage, decoded.Stage)
	assert.Equal(t, original.MaxParallel, decoded.MaxParallel)
	assert.Equal(t, original.DelayAfter, decoded.DelayAfter)
	assert.Equal(t, original.Timeout, decoded.Timeout)
}

func TestRetryPolicy_MarshalJSON(t *testing.T) {
	t.Run("with all fields", func(t *testing.T) {
		policy := RetryPolicy{
			MaxAttempts:        3,
			InitialInterval:    5 * time.Second,
			BackoffCoefficient: 2.0,
			MaxInterval:        1 * time.Minute,
		}

		data, err := json.Marshal(policy)
		require.NoError(t, err)

		var result map[string]any
		require.NoError(t, json.Unmarshal(data, &result))

		assert.Equal(t, "5s", result["initial_interval"])
		assert.Equal(t, "1m0s", result["max_interval"])
	})

	t.Run("with zero max_interval", func(t *testing.T) {
		policy := RetryPolicy{
			MaxAttempts:        3,
			InitialInterval:    5 * time.Second,
			BackoffCoefficient: 2.0,
		}

		data, err := json.Marshal(policy)
		require.NoError(t, err)

		var result map[string]any
		require.NoError(t, json.Unmarshal(data, &result))

		assert.Equal(t, "0s", result["max_interval"])
	})
}

func TestRetryPolicy_UnmarshalJSON(t *testing.T) {
	t.Run("valid retry policy", func(t *testing.T) {
		jsonData := `{"max_attempts":3,"initial_interval":"5s","backoff_coefficient":2.0,"max_interval":"1m"}`

		var policy RetryPolicy
		require.NoError(t, json.Unmarshal([]byte(jsonData), &policy))

		assert.Equal(t, 3, policy.MaxAttempts)
		assert.Equal(t, 5*time.Second, policy.InitialInterval)
		assert.Equal(t, 2.0, policy.BackoffCoefficient)
		assert.Equal(t, 1*time.Minute, policy.MaxInterval)
	})

	t.Run("missing max_interval", func(t *testing.T) {
		jsonData := `{"max_attempts":3,"initial_interval":"5s","backoff_coefficient":2.0}`

		var policy RetryPolicy
		require.NoError(t, json.Unmarshal([]byte(jsonData), &policy))

		assert.Equal(t, time.Duration(0), policy.MaxInterval)
	})

	t.Run("invalid initial_interval", func(t *testing.T) {
		jsonData := `{"max_attempts":3,"initial_interval":"invalid","backoff_coefficient":2.0}`
		var policy RetryPolicy
		assert.Error(t, json.Unmarshal([]byte(jsonData), &policy))
	})

	t.Run("invalid max_interval", func(t *testing.T) {
		jsonData := `{"max_attempts":3,"initial_interval":"5s","backoff_coefficient":2.0,"max_interval":"not-a-duration"}`
		var policy RetryPolicy
		assert.Error(t, json.Unmarshal([]byte(jsonData), &policy))
	})
}

func TestRetryPolicy_MarshalUnmarshal_RoundTrip(t *testing.T) {
	original := RetryPolicy{
		MaxAttempts:        5,
		InitialInterval:    10 * time.Second,
		BackoffCoefficient: 1.5,
		MaxInterval:        5 * time.Minute,
	}

	data, err := json.Marshal(original)
	require.NoError(t, err)

	var decoded RetryPolicy
	require.NoError(t, json.Unmarshal(data, &decoded))

	assert.Equal(t, original.MaxAttempts, decoded.MaxAttempts)
	assert.Equal(t, original.InitialInterval, decoded.InitialInterval)
	assert.Equal(t, original.BackoffCoefficient, decoded.BackoffCoefficient)
	assert.Equal(t, original.MaxInterval, decoded.MaxInterval)
}

func TestSequenceStep_Validate(t *testing.T) {
	t.Run("valid step", func(t *testing.T) {
		step := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         1,
			MaxParallel:   2,
			DelayAfter:    30 * time.Second,
			Timeout:       10 * time.Minute,
			MainOperation: ActionConfig{Name: ActionPowerControl},
		}
		assert.NoError(t, step.Validate())
	})

	t.Run("missing main operation", func(t *testing.T) {
		step := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         1,
			MaxParallel:   2,
		}
		assert.ErrorContains(t, step.Validate(), "main_operation is required")
	})

	t.Run("invalid component type", func(t *testing.T) {
		step := SequenceStep{
			ComponentType: devicetypes.ComponentTypeUnknown,
			Stage:         1,
			MaxParallel:   2,
		}
		assert.Error(t, step.Validate())
	})

	t.Run("invalid stage number", func(t *testing.T) {
		step := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         0,
			MaxParallel:   2,
		}
		assert.Error(t, step.Validate())
	})

	t.Run("negative max_parallel", func(t *testing.T) {
		step := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         1,
			MaxParallel:   -1,
		}
		assert.Error(t, step.Validate())
	})

	t.Run("negative delay_after", func(t *testing.T) {
		step := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         1,
			MaxParallel:   2,
			DelayAfter:    -5 * time.Second,
		}
		assert.Error(t, step.Validate())
	})

	t.Run("negative timeout", func(t *testing.T) {
		step := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         1,
			MaxParallel:   2,
			Timeout:       -10 * time.Minute,
		}
		assert.Error(t, step.Validate())
	})

	t.Run("invalid retry policy", func(t *testing.T) {
		step := SequenceStep{
			ComponentType: devicetypes.ComponentTypeCompute,
			Stage:         1,
			MaxParallel:   2,
			RetryPolicy: &RetryPolicy{
				MaxAttempts:        0,
				InitialInterval:    5 * time.Second,
				BackoffCoefficient: 2.0,
			},
		}
		assert.Error(t, step.Validate())
	})
}

func TestRetryPolicy_Validate(t *testing.T) {
	t.Run("valid policy", func(t *testing.T) {
		policy := RetryPolicy{
			MaxAttempts:        3,
			InitialInterval:    5 * time.Second,
			BackoffCoefficient: 2.0,
			MaxInterval:        1 * time.Minute,
		}
		assert.NoError(t, policy.Validate())
	})

	t.Run("invalid max_attempts", func(t *testing.T) {
		policy := RetryPolicy{
			MaxAttempts:        0,
			InitialInterval:    5 * time.Second,
			BackoffCoefficient: 2.0,
		}
		assert.Error(t, policy.Validate())
	})

	t.Run("zero initial_interval", func(t *testing.T) {
		policy := RetryPolicy{
			MaxAttempts:        3,
			InitialInterval:    0,
			BackoffCoefficient: 2.0,
		}
		assert.Error(t, policy.Validate())
	})

	t.Run("negative initial_interval", func(t *testing.T) {
		policy := RetryPolicy{
			MaxAttempts:        3,
			InitialInterval:    -5 * time.Second,
			BackoffCoefficient: 2.0,
		}
		assert.Error(t, policy.Validate())
	})

	t.Run("invalid backoff_coefficient", func(t *testing.T) {
		policy := RetryPolicy{
			MaxAttempts:        3,
			InitialInterval:    5 * time.Second,
			BackoffCoefficient: 0.5,
		}
		assert.Error(t, policy.Validate())
	})

	t.Run("negative max_interval", func(t *testing.T) {
		policy := RetryPolicy{
			MaxAttempts:        3,
			InitialInterval:    5 * time.Second,
			BackoffCoefficient: 2.0,
			MaxInterval:        -1 * time.Minute,
		}
		assert.Error(t, policy.Validate())
	})
}

func TestRuleDefinition_Validate(t *testing.T) {
	t.Run("valid rule definition", func(t *testing.T) {
		ruleDef := RuleDefinition{
			Version: "v1",
			Steps: []SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   2,
					MainOperation: ActionConfig{Name: ActionPowerControl},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   1,
					MainOperation: ActionConfig{Name: ActionPowerControl},
				},
			},
		}
		assert.NoError(t, ruleDef.Validate())
	})

	t.Run("empty steps", func(t *testing.T) {
		ruleDef := RuleDefinition{Version: "v1", Steps: []SequenceStep{}}
		assert.NoError(t, ruleDef.Validate()) //nolint
	})

	t.Run("duplicate component type in same stage", func(t *testing.T) {
		ruleDef := RuleDefinition{
			Version: "v1",
			Steps: []SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   2,
					MainOperation: ActionConfig{Name: ActionPowerControl},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   1,
					MainOperation: ActionConfig{Name: ActionPowerControl},
				},
			},
		}
		assert.Error(t, ruleDef.Validate())
	})

	t.Run("same component type in different stages is allowed", func(t *testing.T) {
		ruleDef := RuleDefinition{
			Version: "v1",
			Steps: []SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   2,
					MainOperation: ActionConfig{Name: ActionPowerControl},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         2,
					MaxParallel:   1,
					MainOperation: ActionConfig{Name: ActionPowerControl},
				},
			},
		}
		assert.NoError(t, ruleDef.Validate())
	})
}

func TestRuleDefinition_HasApplicableStep(t *testing.T) {
	ruleDef := &RuleDefinition{Steps: []SequenceStep{
		{ComponentType: devicetypes.ComponentTypeNVSwitch},
		{ComponentType: devicetypes.ComponentTypePowerShelf},
	}}
	typeSet := func(types ...devicetypes.ComponentType) map[devicetypes.ComponentType]struct{} {
		result := make(map[devicetypes.ComponentType]struct{}, len(types))
		for _, componentType := range types {
			result[componentType] = struct{}{}
		}
		return result
	}

	tests := []struct {
		name        string
		ruleDef     *RuleDefinition
		targetTypes map[devicetypes.ComponentType]struct{}
		want        bool
	}{
		{
			name:    "one target type overlaps",
			ruleDef: ruleDef,
			targetTypes: typeSet(
				devicetypes.ComponentTypeCompute,
				devicetypes.ComponentTypeNVSwitch,
			),
			want: true,
		},
		{
			name:        "target types do not overlap",
			ruleDef:     ruleDef,
			targetTypes: typeSet(devicetypes.ComponentTypeCompute),
		},
		{name: "empty target scope", ruleDef: ruleDef},
		{
			name:        "nil rule",
			targetTypes: typeSet(devicetypes.ComponentTypeNVSwitch),
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.want, test.ruleDef.HasApplicableStep(test.targetTypes))
		})
	}
}
