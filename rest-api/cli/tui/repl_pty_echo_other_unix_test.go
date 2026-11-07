// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build aix || linux || solaris

package tui

import (
	"os"

	"golang.org/x/sys/unix"
)

func regressionPTYEchoEnabled(file *os.File) (bool, error) {
	state, err := unix.IoctlGetTermios(int(file.Fd()), unix.TCGETS)
	if err != nil {
		return false, err
	}
	return state.Lflag&unix.ECHO != 0, nil
}
