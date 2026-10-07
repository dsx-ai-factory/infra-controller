// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dps

import (
	"context"
	"errors"
	"fmt"
	"strings"
)

var (
	// ErrPowerProfileRequired indicates that a set operation supplied no usable
	// DPS policy name.
	ErrPowerProfileRequired = errors.New("power profile is required")
	// ErrPowerProfileNotFound indicates that DPS ListPolicies did not return the
	// requested policy name.
	ErrPowerProfileNotFound = errors.New("power profile does not exist in DPS")
)

// PolicyCatalog is the normalized set of policy names returned by DPS
// ListPolicies. DPS remains the source of truth; this type does not cache the
// catalogue between requests.
type PolicyCatalog map[string]struct{}

// NewPolicyCatalog trims names, drops empty values, and deduplicates the DPS
// response.
func NewPolicyCatalog(names []string) PolicyCatalog {
	catalog := make(PolicyCatalog, len(names))
	for _, name := range names {
		name = strings.TrimSpace(name)
		if name != "" {
			catalog[name] = struct{}{}
		}
	}
	return catalog
}

// Contains reports whether profileName exactly matches a normalized DPS policy
// name. Callers normalize request values at their API boundary.
func (c PolicyCatalog) Contains(powerProfile string) bool {
	_, ok := c[powerProfile]
	return ok
}

// ValidatePowerProfile normalizes a requested profile and validates it against
// a fresh DPS ListPolicies result. Callers persist and send the returned value,
// not the unnormalized input. Clear and omission operations bypass this helper.
func ValidatePowerProfile(ctx context.Context, provider PolicyProvider, powerProfile string) (string, error) {
	powerProfile = strings.TrimSpace(powerProfile)
	if powerProfile == "" {
		return "", ErrPowerProfileRequired
	}

	profiles, err := provider.ListPowerProfiles(ctx)
	if err != nil {
		return "", fmt.Errorf("list DPS power profiles: %w", err)
	}
	if !NewPolicyCatalog(profiles).Contains(powerProfile) {
		return "", fmt.Errorf("%w: %q", ErrPowerProfileNotFound, powerProfile)
	}
	return powerProfile, nil
}
