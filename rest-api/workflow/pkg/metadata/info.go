// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package metadata contains build-time metadata for the workflow service.
// Version is set by the build system using -ldflags.
package metadata

// Version is the version of the workflow service, set by the build system.
var Version = "dev"
