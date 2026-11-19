// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bmc

import "net/http"

// Wrap sends the gate's requests through f until the returned restore is called.
func Wrap(f func(*http.Transport) http.RoundTripper) (restore func()) {
	old := wrap
	wrap = f
	return func() { wrap = old }
}
