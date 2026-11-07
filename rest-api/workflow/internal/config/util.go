// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"path/filepath"
	"runtime"
)

var (
	_, cur, _, _ = runtime.Caller(0)

	// ProjectRoot describes the folder path of this project
	ProjectRoot = filepath.Join(filepath.Dir(cur), "../..")
)
