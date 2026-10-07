// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processor

import (
	"errors"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/target"
	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
)

// ErrTerminal identifies event-processing failures that cannot succeed on
// retry without changing the input or persisted state.
var ErrTerminal = eventrule.ErrTerminalProcessing

func classifyInventoryError(err error) error {
	if errors.Is(err, inventoryresolver.ErrUnresolvable) {
		return terminalError(err)
	}

	return err
}

func classifyRuleError(err error) error {
	if errors.Is(err, eventrule.ErrInvalidPersistedRule) {
		return terminalError(err)
	}

	return err
}

func isTerminalTargetError(err error) bool {
	return errors.Is(err, target.ErrUnresolvable) ||
		errors.Is(err, inventoryresolver.ErrUnresolvable)
}

func terminalError(err error) error {
	return fmt.Errorf("%w: %w", ErrTerminal, err)
}
