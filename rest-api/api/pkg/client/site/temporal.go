// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"os"

	"sync"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	zlogadapter "logur.dev/adapter/zerolog"
	"logur.dev/logur"

	tsdkClient "go.temporal.io/sdk/client"

	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
	ctemporal "github.com/NVIDIA/infra-controller/rest-api/common/pkg/temporal"
)

// ClientPool contains Temporal clients for different site agents
type ClientPool struct {
	tcfg        *cconfig.TemporalConfig
	IDClientMap map[string]tsdkClient.Client
	mutex       sync.RWMutex
}

// GetClientByID returns a Temporal client for given cluster ID
func (cp *ClientPool) GetClientByID(siteID uuid.UUID) (tsdkClient.Client, error) {
	cp.mutex.RLock()

	client, found := cp.IDClientMap[siteID.String()]
	if found {
		cp.mutex.RUnlock()
		return client, nil
	}

	cp.mutex.RUnlock()

	// A client for the site wasn't found in the site-cache
	// So grab a write-lock so we can create and cache a client.
	cp.mutex.Lock()
	defer cp.mutex.Unlock()

	// Now that we have our exclusive lock,
	// make sure that it wasn't after a previous
	// write-lock holder already updated the client cache.
	client, found = cp.IDClientMap[siteID.String()]
	if found {
		return client, nil
	}

	tLogger := logur.LoggerToKV(zlogadapter.New(zerolog.New(os.Stderr)))

	tOptions, err := ctemporal.ClientOptions(cp.tcfg.GetHostPort(), siteID.String(), cp.tcfg.ClientTLSCfg, tLogger)
	if err != nil {
		log.Panic().Err(err).Str("Temporal Namespace", siteID.String()).
			Msg("failed to build Temporal client options for site")
		return nil, err
	}

	tc, err := tsdkClient.NewLazyClient(tOptions)

	if err != nil {
		log.Panic().Err(err).Str("Temporal Namespace", siteID.String()).
			Msg("failed to create Temporal client for site")
		return nil, err
	}

	cp.IDClientMap[siteID.String()] = tc

	return tc, nil
}

// NewClientPool initializes and returns a new client pool
func NewClientPool(tcfg *cconfig.TemporalConfig) *ClientPool {
	return &ClientPool{
		tcfg:        tcfg,
		IDClientMap: map[string]tsdkClient.Client{},
	}
}
