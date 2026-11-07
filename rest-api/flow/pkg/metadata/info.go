// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package metadata contains build-time metadata for the Flow service.
// These variables are set by the build system using -ldflags.
package metadata

var (
	// Version is the version of Flow, set by the build system
	Version = "dev"
	// BuildTime is the time the binary was built, set by the build system
	BuildTime = "unknown"
	// GitCommit is the git commit hash, set by the build system
	GitCommit = "unknown"
)
