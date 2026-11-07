// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rule

import (
	"encoding/json"
	"fmt"
)

// Selector contains coarse, indexable rule lookup fields.
//
// Kind-specific packages define the supported selector keys and semantics. The
// shared package only verifies that the selector can be stored as JSON and does
// not contain empty top-level keys.
type Selector map[string]any

// Validate checks generic selector constraints.
func (s Selector) Validate() error {
	for key := range s {
		if err := validateTrimmedString("selector key", key); err != nil {
			return err
		}
	}

	if _, err := json.Marshal(s); err != nil {
		return fmt.Errorf("selector must be JSON serializable: %w", err)
	}

	return nil
}
