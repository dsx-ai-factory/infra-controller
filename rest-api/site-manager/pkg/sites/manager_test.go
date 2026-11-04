// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sites

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestManager starts Site Manager over TLS with a serving certificate from a
// Cert Manager stub. Then it creates and reads a Site through the routes.
func TestManager(t *testing.T) {
	s, err := TestManagerCreateSite()
	require.NoError(t, err)
	t.Cleanup(s.Teardown)
	assert.NotEmpty(t, s.UUID1OTP)
}
