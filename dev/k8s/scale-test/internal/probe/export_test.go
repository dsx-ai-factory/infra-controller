// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package probe

import "net/http"

// AllUnitsWithin is AllUnits with its page and unit budget given.
var AllUnitsWithin, ReadBudget = allUnits, [2]int{readPages, readUnits}

// SetTransport sends the calls of an HTTP probe over rt.
func SetTransport(p Probe, rt http.RoundTripper) { p.(*httpProbe).client.Transport = rt }

// HTTPTransport is the built *http.Transport of an HTTP probe.
func HTTPTransport(p Probe) *http.Transport { return p.(*httpProbe).client.Transport.(*http.Transport) }
