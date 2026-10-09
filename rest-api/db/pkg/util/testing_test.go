// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestGetTestDBParams_ExplicitEnvironmentOverridesCI(t *testing.T) {
	t.Setenv("CI", "true")
	t.Setenv("PGHOST", "/var/run/postgresql")
	t.Setenv("PGPORT", "6432")
	t.Setenv("PGUSER", "root")
	t.Setenv("PGPASSWORD", "test-password")

	assert.Equal(t, TestDBConfig{
		Host: "/var/run/postgresql", Port: 6432, Name: "nicotest",
		User: "root", Password: "test-password",
	}, getTestDBParams())
}
