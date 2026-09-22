// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package codec

import (
	"encoding/json"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// MarshalAction encodes a current domain action using the latest persistence
// format.
func MarshalAction(action eventrule.Action) (json.RawMessage, error) {
	if err := action.Validate(); err != nil {
		return nil, err
	}

	return marshalActionV1(action)
}

// UnmarshalAction decodes any supported persisted action into the current
// domain.
func UnmarshalAction(data json.RawMessage) (eventrule.Action, error) {
	version, err := decodeVersion(data, "event action")
	if err != nil {
		return eventrule.Action{}, err
	}

	var action eventrule.Action
	switch version {
	case actionVersionV1:
		action, err = unmarshalActionV1(data)
		if err != nil {
			return eventrule.Action{}, err
		}
	default:
		return eventrule.Action{}, fmt.Errorf(
			"unknown event action version %d",
			version,
		)
	}

	if err := action.Validate(); err != nil {
		return eventrule.Action{}, fmt.Errorf("validate event action: %w", err)
	}

	return action, nil
}
