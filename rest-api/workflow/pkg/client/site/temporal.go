// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"os"

	"fmt"
	"sync"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	zlogadapter "logur.dev/adapter/zerolog"
	"logur.dev/logur"

	"go.opentelemetry.io/otel"
	tsdkClient "go.temporal.io/sdk/client"
	"go.temporal.io/sdk/contrib/opentelemetry"
	tsdkConverter "go.temporal.io/sdk/converter"
	"go.temporal.io/sdk/interceptor"

	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
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
	cp.mutex.Lock()
	defer cp.mutex.Unlock()

	tLogger := logur.LoggerToKV(zlogadapter.New(zerolog.New(os.Stderr)))

	// Every SiteTaskQueue workflow leaves through this client.
	var tInterceptors []interceptor.ClientInterceptor
	otelInterceptor, oerr := opentelemetry.NewTracingInterceptor(
		opentelemetry.TracerOptions{TextMapPropagator: otel.GetTextMapPropagator()})
	if oerr != nil {
		return nil, fmt.Errorf("creating Temporal tracing interceptor: %w", oerr)
	}
	tInterceptors = append(tInterceptors, otelInterceptor)

	tc, err := tsdkClient.NewLazyClient(tsdkClient.Options{
		HostPort:  fmt.Sprintf("%v:%v", cp.tcfg.Host, cp.tcfg.Port),
		Namespace: siteID.String(),
		ConnectionOptions: tsdkClient.ConnectionOptions{
			TLS: cp.tcfg.ClientTLSCfg,
		},
		DataConverter: tsdkConverter.NewCompositeDataConverter(
			tsdkConverter.NewNilPayloadConverter(),
			tsdkConverter.NewByteSlicePayloadConverter(),
			tsdkConverter.NewProtoJSONPayloadConverterWithOptions(tsdkConverter.ProtoJSONPayloadConverterOptions{
				AllowUnknownFields: true,
			}),
			tsdkConverter.NewProtoPayloadConverter(),
			tsdkConverter.NewJSONPayloadConverter(),
		),
		Logger:       tLogger,
		Interceptors: tInterceptors,
	})

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
