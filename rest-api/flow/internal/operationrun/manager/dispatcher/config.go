// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import "time"

const (
	defaultPollInterval         = 10 * time.Second
	defaultFetchBatch           = 10
	defaultClaimLease           = 30 * time.Second
	defaultSubmitPersistTimeout = 5 * time.Second
)

// Config holds dispatcher polling and recovery settings.
type Config struct {
	PollInterval         time.Duration
	FetchBatch           int
	ClaimLease           time.Duration
	SubmitPersistTimeout time.Duration
}

func (c Config) withDefaults() Config {
	if c.PollInterval <= 0 {
		c.PollInterval = defaultPollInterval
	}

	if c.FetchBatch <= 0 {
		c.FetchBatch = defaultFetchBatch
	}

	if c.ClaimLease <= 0 {
		c.ClaimLease = defaultClaimLease
	}

	if c.SubmitPersistTimeout <= 0 {
		c.SubmitPersistTimeout = defaultSubmitPersistTimeout
	}

	return c
}
