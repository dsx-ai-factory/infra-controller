// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package codec

import (
	"encoding/json"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// MarshalExecutionPlan encodes a current execution plan using the latest
// persistence format.
func MarshalExecutionPlan(plan eventrule.ExecutionPlan) (json.RawMessage, error) {
	if err := eventrule.ValidateExecutionPlan(plan); err != nil {
		return nil, err
	}

	return marshalExecutionPlanV1(plan)
}

// UnmarshalExecutionPlan decodes any supported persisted execution plan into
// the current domain.
func UnmarshalExecutionPlan(data json.RawMessage) (eventrule.ExecutionPlan, error) {
	version, err := decodeVersion(data, "execution plan")
	if err != nil {
		return nil, err
	}

	var plan eventrule.ExecutionPlan
	switch version {
	case executionPlanVersionV1:
		plan, err = unmarshalExecutionPlanV1(data)
		if err != nil {
			return nil, err
		}
	default:
		return nil, fmt.Errorf("unknown execution plan version %d", version)
	}

	if err := eventrule.ValidateExecutionPlan(plan); err != nil {
		return nil, fmt.Errorf("validate execution plan: %w", err)
	}

	return plan, nil
}
