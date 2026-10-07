// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package codec

import (
	"encoding/json"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// MarshalPolicy encodes a current domain policy using the latest persistence
// format.
func MarshalPolicy(policy eventrule.Policy) (json.RawMessage, error) {
	if err := policy.Validate(); err != nil {
		return nil, err
	}

	return marshalPolicyV1(policy)
}

// UnmarshalPolicy decodes any supported persisted policy into the current
// domain.
func UnmarshalPolicy(data json.RawMessage) (eventrule.Policy, error) {
	version, err := decodeVersion(data, "event policy")
	if err != nil {
		return eventrule.Policy{}, err
	}

	var policy eventrule.Policy
	switch version {
	case policyVersionV1:
		policy, err = unmarshalPolicyV1(data)
		if err != nil {
			return eventrule.Policy{}, err
		}
	default:
		return eventrule.Policy{}, fmt.Errorf("unknown event policy version %d", version)
	}

	if err := policy.Validate(); err != nil {
		return eventrule.Policy{}, fmt.Errorf("validate event policy: %w", err)
	}

	return policy, nil
}
