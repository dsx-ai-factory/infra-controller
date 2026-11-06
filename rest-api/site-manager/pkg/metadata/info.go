// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package metadata holds Site Manager build metadata.
package metadata

// Version is the Site Manager release version, set by the build system with
// -X. Builds without version metadata report dev.
var Version = "dev"
