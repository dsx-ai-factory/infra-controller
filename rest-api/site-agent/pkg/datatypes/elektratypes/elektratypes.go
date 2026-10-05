// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package elektratypes

import (
	"os"

	"github.com/dsx-ai-factory/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/dsx-ai-factory/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
	"github.com/rs/zerolog"
	"go.uber.org/atomic"
)

// Elektra is the main struct for the Elektra plugin
type Elektra struct {
	// Main structure of Elektra
	// All information is contained in this structure
	Managers *managertypes.Managers
	Conf     *conftypes.Config
	// HealthStatus current health state
	HealthStatus atomic.Uint64
	Version      string
	Log          zerolog.Logger
}

// NewElektraTypes - create new Elektra Type
func NewElektraTypes() *Elektra {
	return &Elektra{
		Version:  "0.0.1",
		Managers: managertypes.NewManagerType(),
		Conf:     conftypes.NewConfType(),
		Log:      zerolog.New(os.Stderr).With().Timestamp().Logger(),
	}
}
