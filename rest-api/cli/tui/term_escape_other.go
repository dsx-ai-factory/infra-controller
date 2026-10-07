// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build !(aix || darwin || dragonfly || freebsd || linux || netbsd || openbsd || solaris)

package tui

import "os"

// Non-Unix terminals do not use the ANSI three-byte arrow sequences handled
// by ReadKey. Treat Escape as a standalone cancellation key without waiting.
func readEscapeSequence(_ *os.File, _ []byte) (int, error) {
	return 0, nil
}
