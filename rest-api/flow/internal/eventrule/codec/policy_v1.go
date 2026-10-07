// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package codec

import (
	"encoding/json"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

const policyVersionV1 = 1

type policyV1 struct {
	Version int               `json:"version"`
	Actions []json.RawMessage `json:"actions"`
}

func marshalPolicyV1(policy eventrule.Policy) (json.RawMessage, error) {
	persisted := policyV1{
		Version: policyVersionV1,
	}
	if policy.Actions != nil {
		persisted.Actions = make([]json.RawMessage, len(policy.Actions))
	}

	for i, action := range policy.Actions {
		encodedAction, err := MarshalAction(action)
		if err != nil {
			return nil, fmt.Errorf("actions[%d]: %w", i, err)
		}

		persisted.Actions[i] = encodedAction
	}

	encoded, err := json.Marshal(persisted)
	if err != nil {
		return nil, fmt.Errorf("encode event policy v1: %w", err)
	}

	return encoded, nil
}

func unmarshalPolicyV1(data json.RawMessage) (eventrule.Policy, error) {
	var persisted policyV1
	if err := decodeStrict(data, &persisted); err != nil {
		return eventrule.Policy{}, fmt.Errorf("decode event policy v1: %w", err)
	}

	if persisted.Version != policyVersionV1 {
		return eventrule.Policy{}, fmt.Errorf(
			"unexpected event policy v1 version %d",
			persisted.Version,
		)
	}

	policy := eventrule.Policy{}
	if persisted.Actions != nil {
		policy.Actions = make([]eventrule.Action, len(persisted.Actions))
	}

	for i, action := range persisted.Actions {
		decodedAction, err := UnmarshalAction(action)
		if err != nil {
			return eventrule.Policy{}, fmt.Errorf("actions[%d]: %w", i, err)
		}

		policy.Actions[i] = decodedAction
	}

	return policy, nil
}
