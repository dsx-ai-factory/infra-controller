// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package elektra

import (
	zlog "github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/config"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
)

// Interface - Managers' interface
type Interface interface {
	Managers() managers.Manager
}

// Elektra - Managers struct
type Elektra struct {
	manager *managers.Manager
}

// Init - initializes the cluster
func (Cluster *Elektra) Init() (err error) {
	zlog.Info().Msg("Elektra: Initializing Elektra cluster")
	Cluster.Managers().Init()
	return nil
}

// Start () Start the Cluster
func (Cluster *Elektra) Start() (err error) {
	zlog.Info().Msg("Elektra: Starting Elektra cluster")
	Cluster.Managers().Start()
	return nil
}

// Managers () Instantiate the Managers
func (Cluster *Elektra) Managers() *managers.Manager {
	return Cluster.manager
}

// NewElektraAPI - Instantiate new struct
func NewElektraAPI(superElektra *elektratypes.Elektra, utMode bool) (*Elektra, error) {
	zlog.Info().Msg("Elektra: Initializing Config Manager")
	var eb Elektra
	var err error
	// Initialize Global Config
	// Load configuration
	if superElektra != nil {
		// Configuration
		zlog.Info().Msg("Elektra: Loading configuration")
		superElektra.Conf = config.NewElektraConfig(utMode)
		eb.manager, err = managers.NewInstance(superElektra)
		zlog.Info().Interface("config", superElektra.Conf).Msg("Elektra: Config Manager initialized")
	}

	return &eb, err
}
