// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package temporal

import (
	"errors"
	"os"
	"time"

	dynamictls "github.com/NVIDIA/infra-controller/rest-api/common/pkg/tls"
	"go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/endpoint"
	ctemporal "github.com/NVIDIA/infra-controller/rest-api/common/pkg/temporal"
)

const (
	defaultKeepAliveTime    = 10 * time.Second
	defaultKeepAliveTimeout = 60 * time.Second
)

type Client struct {
	config     Config
	options    client.Options
	client     client.Client
	dynamicTLS *dynamictls.DynTLSCfg
}

type Config struct {
	Endpoint   endpoint.Config
	EnableTLS  bool
	ServerName string
	Namespace  string
}

func (c *Config) Validate() error {
	if err := c.Endpoint.Validate(); err != nil {
		return err
	}

	if c.EnableTLS {
		if c.ServerName == "" {
			return errors.New("server name is required")
		}

		if c.Endpoint.CACertificatePath == "" {
			return errors.New("CA certificate path is required")
		}

		if _, err := os.Stat(c.Endpoint.CACertificatePath); os.IsNotExist(err) { //nolint
			return errors.New("CA certificate path does not exist")
		}
	}

	return nil
}

func New(c Config) (*Client, error) {
	if err := c.Validate(); err != nil {
		return nil, err
	}

	tlsConfig, dynamicConfig, err := buildTLSConfig(c)
	if err != nil {
		return nil, err
	}

	options := client.Options{
		HostPort:  c.Endpoint.Target(),
		Namespace: c.Namespace,
		ConnectionOptions: client.ConnectionOptions{
			TLS:              tlsConfig,
			KeepAliveTime:    defaultKeepAliveTime,
			KeepAliveTimeout: defaultKeepAliveTimeout,
		},
	}
	options, err = ctemporal.ConfigureClientOptions(options)
	if err != nil {
		if dynamicConfig != nil {
			dynamicConfig.Close()
		}
		return nil, err
	}

	client, err := client.Dial(options)
	if err != nil {
		if dynamicConfig != nil {
			dynamicConfig.Close()
		}
		return nil, err
	}

	return &Client{config: c, options: options, client: client, dynamicTLS: dynamicConfig}, nil
}

func (c *Client) Client() client.Client {
	return c.client
}

// Close closes the Temporal connection and stops certificate refreshes.
func (c *Client) Close() {
	c.client.Close()
	if c.dynamicTLS != nil {
		c.dynamicTLS.Close()
	}
}
