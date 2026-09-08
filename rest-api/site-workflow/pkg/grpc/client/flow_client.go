// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"crypto/md5"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"os"
	"strconv"
	"sync/atomic"
	"time"

	"github.com/rs/zerolog/log"

	grpcmw "github.com/grpc-ecosystem/go-grpc-middleware"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"

	"go.opentelemetry.io/contrib/instrumentation/google.golang.org/grpc/otelgrpc"
	"go.opentelemetry.io/otel"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// Errors
var (
	ErrFlowGrpcClientInvalidAddress    = errors.New("FlowGrpcClient: invalid address")
	ErrFlowGrpcClientInvalidDialOpts   = errors.New("FlowGrpcClient: invalid dial options")
	ErrFlowGrpcClientInvalidSecureOpts = errors.New("FlowGrpcClient: invalid secure options")
	ErrFlowGrpcClientInvalidServerCA   = errors.New("FlowGrpcClient: invalid server CA")
	ErrFlowGrpcClientInvalidClientCA   = errors.New("FlowGrpcClient: invalid client CA")
	ErrFlowGrpcClientInvalidClientKey  = errors.New("FlowGrpcClient: invalid client key")
	ErrFlowGrpcClientInvalidClientCert = errors.New("FlowGrpcClient: invalid client cert")
	ErrFlowGrpcClientNotConnected      = errors.New("FlowGrpcClient: gRPC client is not connected to the server")
)

// SecureOptions is the enum for the secure options
type FlowGrpcClientSecureOptions int

const (
	// FlowInsecureGrpc is the insecure dial option
	FlowInsecureGrpc FlowGrpcClientSecureOptions = iota
	// FlowServerTLS is the secure dial option for server tls
	FlowServerTLS
	// FlowMutualTLS for mutual tls
	FlowMutualTLS

	// defaultCheckCertificateIntervalSeconds is the default interval to check for certificate changes
	defaultCheckFlowCertificateIntervalSeconds = 15 * 60 // 15 minutes in seconds

	// gRPC client default dial timeout
	defaultFlowGrpcDialTimeoutSeconds = 5 // 5 seconds

	// FlowGrpcConnectionRetryTimeout is the maximum time to retry establishing a Flow gRPC connection.
	FlowGrpcConnectionRetryTimeout = 15 * time.Minute
	// FlowGrpcConnectionBackoffInitial is the initial delay between connection retries.
	FlowGrpcConnectionBackoffInitial = 5 * time.Second
	// FlowGrpcConnectionBackoffMax is the maximum delay between connection retries.
	FlowGrpcConnectionBackoffMax = 60 * time.Second
)

// FlowClientConfig is the data structure for the client configuration
type FlowGrpcClientConfig struct {
	// The address of the server <host>:<port>
	Address string
	// Secure flag
	Secure FlowGrpcClientSecureOptions
	// Skip Server Auth
	SkipServerAuth bool
	// The TLS certificate for the server
	ServerCAPath string
	// The TLS certificate for the client
	ClientCertPath string
	// The TLS key for the client
	ClientKeyPath string
	// client metrics interface
	ClientMetrics Metrics
}

// NewFlowGrpcClient creates a new Flow gRPC client, this is called by Site Agent startup code and cert reload routine
// Caller is responsible for retrying connection failure
func NewFlowGrpcClient(config *FlowGrpcClientConfig) (client *FlowGrpcClient, err error) {
	// Validate the config
	if config.Address == "" {
		log.Error().Err(ErrFlowGrpcClientInvalidAddress).Msg("FlowGrpcClient: No server address configured")
		return nil, ErrFlowGrpcClientInvalidAddress
	}
	client = &FlowGrpcClient{}

	switch config.Secure {
	case FlowInsecureGrpc:
		// No secure options
		// Default option
		// connect with plain TCP
		log.Debug().Msg("FlowGrpcClient: Using insecure gRPC connection. WARNING: This should not be used in Production)")
		client.dialOpts = append(client.dialOpts, grpc.WithTransportCredentials(insecure.NewCredentials()))
	case FlowServerTLS:
		log.Debug().Msg("FlowGrpcClient: Using server TLS connection")
		// Validate the config contains server ca path
		if config.ServerCAPath == "" {
			log.Error().Err(ErrFlowGrpcClientInvalidServerCA).Msg("FlowGrpcClient: No server CA path configured")
			return nil, ErrFlowGrpcClientInvalidServerCA
		}
		if config.SkipServerAuth {
			// Server TLS
			// connect with TLS but not mutual TLS
			log.Info().Msg("FlowGrpcClient: Skipping server auth in TLS. WARNING: This should not be used in Production)")
			tlsConfig := &tls.Config{
				InsecureSkipVerify: true,
			}
			// Load the server ca
			_, err := credentials.NewClientTLSFromFile(config.ServerCAPath, "")
			if err != nil {
				log.Error().Err(err).Msg("FlowGrpcClient: Failed to load server CA cert")
				return nil, err
			}

			// Create client dial option
			// Append the dial option
			client.dialOpts = append(client.dialOpts, grpc.WithTransportCredentials(credentials.NewTLS(tlsConfig)))

		} else {
			// Server TLS
			// connect with TLS but not mutual TLS
			// Load the server ca
			creds, err := credentials.NewClientTLSFromFile(config.ServerCAPath, "")
			if err != nil {
				log.Error().Err(err).Msg("FlowGrpcClient: Failed to load server CA cert")
				return nil, err
			}
			// Append the dial option
			client.dialOpts = append(client.dialOpts, grpc.WithTransportCredentials(creds))
		}
	case FlowMutualTLS:
		// Mutual TLS
		// connect with mutual TLS
		log.Debug().Msg("FlowGrpcClient: Using mutual TLS connection")
		// 1. Load the client certificates
		clientCert, err := tls.LoadX509KeyPair(config.ClientCertPath, config.ClientKeyPath)
		if err != nil {
			log.Error().Err(err).Msg("FlowGrpcClient: Failed to load client certificates")
			return nil, err
		}
		// 2. Load the Trust chain, root ca
		cabytes, err := os.ReadFile(config.ServerCAPath)
		if err != nil {
			log.Error().Err(err).Msg("FlowGrpcClient: Failed to load Root CA certificates")

			return nil, err
		}
		capool := x509.NewCertPool()
		if !capool.AppendCertsFromPEM(cabytes) {
			return nil, fmt.Errorf("FlowGrpcClient: Failed to append CA cert to CA pool")
		}
		// Use GetClientCertificate (not Certificates) to unconditionally present
		// the client cert. With Certificates, Go's TLS stack only selects a cert
		// whose issuer matches the acceptable CA list from the server's
		// CertificateRequest; when no match is found it silently sends no cert,
		// causing the server to reject with "tls: certificate required".
		// GetClientCertificate bypasses that matching and always returns the cert,
		// leaving verification to the server — the same approach used in
		// rest-api/flow/pkg/certs/certs.go TLSConfig().
		mutualTLSConfig := &tls.Config{
			GetClientCertificate: func(*tls.CertificateRequestInfo) (*tls.Certificate, error) {
				return &clientCert, nil
			},
			RootCAs: capool,
		}
		creds := credentials.NewTLS(mutualTLSConfig)

		// Append to the dial option
		client.dialOpts = append(client.dialOpts, grpc.WithTransportCredentials(creds))

	default:
		log.Error().Err(ErrFlowGrpcClientInvalidSecureOpts).Msg("FlowGrpcClient: Invalid dial options")
		return nil, ErrFlowGrpcClientInvalidSecureOpts
	}

	// Configure interceptors
	var unaryInterceptors []grpc.UnaryClientInterceptor
	if config.ClientMetrics != nil {
		unaryInterceptors = append(unaryInterceptors, newGrpcUnaryMetricsInterceptor(config.ClientMetrics))
	}
	var streamInterceptors []grpc.StreamClientInterceptor
	if config.ClientMetrics != nil {
		streamInterceptors = append(streamInterceptors, newGrpcStreamMetricsInterceptor(config.ClientMetrics))
	}
	// Unconditional: was gated on LS_SERVICE_NAME, which no chart sets.
	handler := otelgrpc.NewClientHandler(otelgrpc.WithPropagators(otel.GetTextMapPropagator()))
	client.dialOpts = append(client.dialOpts, grpc.WithStatsHandler(handler))
	if len(unaryInterceptors) > 0 {
		client.dialOpts = append(client.dialOpts, grpc.WithUnaryInterceptor(grpcmw.ChainUnaryClient(unaryInterceptors...)))
	}
	if len(streamInterceptors) > 0 {
		client.dialOpts = append(client.dialOpts, grpc.WithStreamInterceptor(grpcmw.ChainStreamClient(streamInterceptors...)))
	}

	// Create the client connection
	client.conn, err = grpc.NewClient(config.Address, client.dialOpts...)
	if err != nil {
		log.Error().Err(err).Msg("FlowGrpcClient: Failed to initialize gRPC client")
		return nil, err
	}
	log.Info().Msg("FlowGrpcClient: gRPC client initialized")

	// Create Flow client
	client.grpcServiceClient = flowv1.NewFlowClient(client.conn)
	log.Info().Msg("FlowGrpcClient: gRPC service client created")

	// Check the version of the server
	ctx, cancel := context.WithDeadline(context.Background(), time.Now().Add(time.Duration(defaultFlowGrpcDialTimeoutSeconds)*time.Second))
	defer cancel()
	_, err = client.grpcServiceClient.Version(ctx, &flowv1.VersionRequest{})
	if err != nil {
		log.Error().Err(err).Msg("FlowGrpcClient: Failed to get version from server")
		return nil, fmt.Errorf("FlowGrpcClient: Failed to get version from server: %w", err)
	}

	log.Info().Msg("FlowGrpcClient: Successfully connected to server")

	return client, nil
}

// FlowGrpcClient is the data structure for the client
type FlowGrpcClient struct {
	// The client connection
	conn *grpc.ClientConn
	// gRPC dial options
	dialOpts []grpc.DialOption
	// gRPC service client interface
	grpcServiceClient flowv1.FlowClient
}

// Close gracefully shuts down the client's gRPC connection.
func (fg *FlowGrpcClient) Close() error {
	if fg.conn != nil {
		// Close the grpc.ClientConn.
		return fg.conn.Close()
	}
	return nil
}

// GrpcService client getter
func (client *FlowGrpcClient) GrpcServiceClient() flowv1.FlowClient {
	return client.grpcServiceClient
}

// FlowAtomicClient is an atomic wrapper around the FlowClient
type FlowGrpcAtomicClient struct {
	Config  *FlowGrpcClientConfig
	value   *atomic.Value
	version atomic.Int64
}

// Version returns the current version of the FlowGrpcClient
func (fgac *FlowGrpcAtomicClient) Version() int64 {
	return fgac.version.Load()
}

// SwapClient atomically replaces the current FlowGrpcClient with a new one,
// returning the old client for the caller to manage.
func (fgac *FlowGrpcAtomicClient) SwapClient(newClient *FlowGrpcClient) *FlowGrpcClient {

	// Atomically replace the current client with the new one and return the old client.
	oldClientInterface := fgac.value.Swap(newClient)

	// Type assert the returned value to *FlowGrpcClient.
	// This should always succeed if the correct type was stored initially.
	oldClient, ok := oldClientInterface.(*FlowGrpcClient)
	if !ok {
		log.Error().Msg("FlowGrpcAtomicClient: Type assertion failed for the old client")
		return nil
	}

	// Increment the version number
	fgac.version.Add(1)

	return oldClient
}

// GetClient returns the current version of FlowGrpcClient from the atomic value.
// Returns nil if the client has not been initialized yet.
func (fgac *FlowGrpcAtomicClient) GetClient() *FlowGrpcClient {
	v := fgac.value.Load()
	if v == nil {
		return nil
	}
	client, _ := v.(*FlowGrpcClient)

	return client
}

// GrpcServiceClient returns the underlying Flow gRPC service. Returns ErrFlowGrpcClientNotConnected
// if the client has not been initialized or is not currently connected.
// Prefer this over GetClient() + manual nil-check + .GrpcServiceClient() at call sites.
func (fgac *FlowGrpcAtomicClient) GrpcServiceClient() (flowv1.FlowClient, error) {
	client := fgac.GetClient()
	if client == nil {
		return nil, ErrFlowGrpcClientNotConnected
	}
	// It's true that NewFlowClient always populates the inner flow field, BUT,
	// guard against zero-value FlowClient instances slipping in via direct
	// construction. Without this, a misconstructed wrapper would yield (nil,
	// nil) and break things.
	grpcServiceClient := client.GrpcServiceClient()
	if grpcServiceClient == nil {
		return nil, ErrFlowGrpcClientNotConnected
	}
	return grpcServiceClient, nil
}

// CheckAndReloadCerts continuously monitors the TLS certificates for changes.
// If a change is detected, it reinitializes the FlowGrpcClient with the new certificates to ensure secure communication.
func (fgac *FlowGrpcAtomicClient) CheckAndReloadCerts(initialClientCertMD5, initialServerCAMD5 []byte) {
	// Initialize contextual logger
	logger := log.With().Str("Component", "FlowGrpc").Str("Operation", "CheckAndReloadCerts").Logger()

	ticker := time.NewTicker(getFlowGrpcCertificateCheckInterval())
	defer ticker.Stop()

	lastClientCertMD5, lastServerCAMD5 := initialClientCertMD5, initialServerCAMD5
	var (
		reloadStarted time.Time
		reloadBackoff time.Duration
	)

	for range ticker.C {
		changed, newClientMD5, newServerMD5, err := fgac.CheckCertificates(lastClientCertMD5, lastServerCAMD5)
		if err != nil {
			logger.Error().Err(err).Msg("Error checking certificates for changes")
			continue
		}

		if !changed && reloadBackoff == 0 {
			continue
		}

		if reloadBackoff == 0 {
			reloadStarted = time.Now()
			reloadBackoff = FlowGrpcConnectionBackoffInitial
		} else if time.Since(reloadStarted) >= FlowGrpcConnectionRetryTimeout {
			panic(fmt.Errorf("Flow gRPC: failed to reinitialize gRPC client with new certificates within %s",
				FlowGrpcConnectionRetryTimeout))
		}

		newClient, err := NewFlowGrpcClient(fgac.Config)
		if err != nil {
			if time.Since(reloadStarted) >= FlowGrpcConnectionRetryTimeout {
				panic(fmt.Errorf("Flow gRPC: failed to reinitialize gRPC client with new certificates within %s: %w",
					FlowGrpcConnectionRetryTimeout, err))
			}
			logger.Error().Err(err).Dur("RetryIn", reloadBackoff).Msg("Failed to reinitialize gRPC client with new certificates, retrying")
			ticker.Reset(reloadBackoff)
			reloadBackoff *= 2
			if reloadBackoff > FlowGrpcConnectionBackoffMax {
				reloadBackoff = FlowGrpcConnectionBackoffMax
			}
			continue
		}

		reloadBackoff = 0

		// Atomically update the client instance and get the old one.
		oldClient := fgac.SwapClient(newClient)

		// Delayed closure of the old client.
		go func(clientToClose *FlowGrpcClient) {
			// Delay the closure to allow ongoing client requests to complete.
			time.Sleep(10 * time.Second) // Adjust the delay as needed.

			// Ensure the client exists and has a connection to close.
			if clientToClose != nil {
				if err := clientToClose.Close(); err != nil {
					log.Error().Err(err).Msg("Error closing old FlowGrpcClient connection")
				}
			}
		}(oldClient)

		logger.Info().Msg("gRPC client successfully reinitialized with new certificates")

		// Update the stored MD5 hashes with the new ones for the next comparison.
		lastClientCertMD5, lastServerCAMD5 = newClientMD5, newServerMD5

		// Reset the ticker interval to the default
		ticker.Reset(getFlowGrpcCertificateCheckInterval())
	}
}

// GetInitialCertMD5 retrieves the MD5 hash of the initial set of certificate that the client is Using
func (fgac *FlowGrpcAtomicClient) GetInitialCertMD5() (clientCertMD5, serverCAMD5 []byte, err error) {
	// Load and hash the client certificate
	clientCertBytes, err := os.ReadFile(fgac.Config.ClientCertPath)
	if err != nil {
		return nil, nil, err
	}
	clientCertMD5Hash := md5.Sum(clientCertBytes)
	clientCertMD5 = clientCertMD5Hash[:]

	// Load and hash the server CA certificate using os.ReadFile
	serverCABytes, err := os.ReadFile(fgac.Config.ServerCAPath)
	if err != nil {
		return nil, nil, err
	}
	serverCAMD5Hash := md5.Sum(serverCABytes)
	serverCAMD5 = serverCAMD5Hash[:]

	return clientCertMD5, serverCAMD5, nil
}

// CheckCertificates checks if the client and server CA certificates have changed
func (fgac *FlowGrpcAtomicClient) CheckCertificates(lastClientCertMD5, lastServerCAMD5 []byte) (bool, []byte, []byte, error) {
	// Load and hash the client certificate using os.ReadFile
	clientCertBytes, err := os.ReadFile(fgac.Config.ClientCertPath)
	if err != nil {
		return false, lastClientCertMD5, lastServerCAMD5, err
	}
	clientCertMD5 := md5.Sum(clientCertBytes)

	// Load and hash the server CA certificate using os.ReadFile
	serverCABytes, err := os.ReadFile(fgac.Config.ServerCAPath)
	if err != nil {
		return false, lastClientCertMD5, lastServerCAMD5, err
	}
	serverCAMD5 := md5.Sum(serverCABytes)

	// Check if either certificate has changed
	if !equalMD5(lastClientCertMD5, clientCertMD5[:]) || !equalMD5(lastServerCAMD5, serverCAMD5[:]) {
		return true, clientCertMD5[:], serverCAMD5[:], nil
	}

	return false, lastClientCertMD5, lastServerCAMD5, nil
}

// NewFlowAtomicClient creates a new FlowAtomicClient
func NewFlowGrpcAtomicClient(config *FlowGrpcClientConfig) *FlowGrpcAtomicClient {
	// Create the atomic value
	atomicClient := &FlowGrpcAtomicClient{
		Config:  config,
		value:   &atomic.Value{},
		version: atomic.Int64{},
	}

	return atomicClient
}

func getFlowGrpcCertificateCheckInterval() time.Duration {
	value, ok := os.LookupEnv("FLOW_GRPC_CERT_CHECK_INTERVAL")
	if !ok {
		return defaultCheckFlowCertificateIntervalSeconds * time.Second
	}
	interval, err := strconv.Atoi(value)
	if err != nil {
		log.Error().Err(err).Str("FLOW_GRPC_CERT_CHECK_INTERVAL", value).Msg("Invalid FLOW_GRPC_CERT_CHECK_INTERVAL value; using default.")
		return defaultCheckFlowCertificateIntervalSeconds * time.Second
	}
	if interval <= 0 {
		log.Error().Int("FLOW_GRPC_CERT_CHECK_INTERVAL", interval).Msg("FLOW_GRPC_CERT_CHECK_INTERVAL must be > 0; using default.")
		return defaultCheckFlowCertificateIntervalSeconds * time.Second
	}
	return time.Duration(interval) * time.Second
}
