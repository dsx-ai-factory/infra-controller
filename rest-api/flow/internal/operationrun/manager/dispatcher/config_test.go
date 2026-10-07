// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"testing"
	"time"

	"github.com/stretchr/testify/require"
)

func TestConfigWithDefaultsSetsSubmitPersistTimeout(t *testing.T) {
	cfg := Config{}.withDefaults()

	require.Equal(t, defaultPollInterval, cfg.PollInterval)
	require.Equal(t, defaultFetchBatch, cfg.FetchBatch)
	require.Equal(t, defaultClaimLease, cfg.ClaimLease)
	require.Equal(t, defaultSubmitPersistTimeout, cfg.SubmitPersistTimeout)
}

func TestConfigWithDefaultsPreservesSubmitPersistTimeout(t *testing.T) {
	timeout := 42 * time.Millisecond

	cfg := Config{
		SubmitPersistTimeout: timeout,
	}.withDefaults()

	require.Equal(t, timeout, cfg.SubmitPersistTimeout)
}
