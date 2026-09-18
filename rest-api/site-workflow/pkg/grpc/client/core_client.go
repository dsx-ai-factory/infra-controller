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

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// Errors
var (
	ErrCoreGrpcClientInvalidAddress    = errors.New("CoreGrpcClient: invalid address")
	ErrCoreGrpcClientInvalidDialOpts   = errors.New("CoreGrpcClient: invalid dial options")
	ErrCoreGrpcClientInvalidSecureOpts = errors.New("CoreGrpcClient: invalid secure options")
	ErrCoreGrpcClientInvalidServerCA   = errors.New("CoreGrpcClient: invalid server CA")
	ErrCoreGrpcClientInvalidClientCA   = errors.New("CoreGrpcClient: invalid client CA")
	ErrCoreGrpcClientInvalidClientKey  = errors.New("CoreGrpcClient: invalid client key")
	ErrCoreGrpcClientInvalidClientCert = errors.New("CoreGrpcClient: invalid client cert")
	ErrCoreGrpcClientNotConnected      = errors.New("CoreGrpcClient: gRPC client is not connected to the server")
)

// SecureOptions is the enum for the secure options
type SecureOptions int

const (
	// InsecureGrpc is the insecure dial option
	InsecureGrpc SecureOptions = iota
	// ServerTLS is the secure dial option for server tls
	ServerTLS
	// MutualTLS for mutual tls
	MutualTLS

	// defaultCheckCertificateIntervalSeconds is the default interval to check for certificate changes
	defaultCheckCertificateIntervalSeconds = 15 * 60 // 15 minutes in seconds

	// gRPC client default dial timeout
	defaultCoreGrpcDialTimeoutSeconds = 5 // 5 seconds

	// coreGrpcMaxRecvMsgSize raises the Go gRPC 4 MiB default. Instance inventory already
	// returns ~7.8 MB for a page of 100, and a ceiling near that size leaves the inventory
	// pager's site page ladder in permanent use, where every rejected response still costs Core
	// a full query and serialization. This is a limit rather than a reservation, so the headroom
	// only consumes memory once a response arrives to fill it.
	coreGrpcMaxRecvMsgSize = 32 * 1024 * 1024

	// CoreGrpcConnectionRetryTimeout is the maximum time to retry establishing a Core gRPC connection.
	CoreGrpcConnectionRetryTimeout = 15 * time.Minute
	// CoreGrpcConnectionBackoffInitial is the initial delay between connection retries.
	CoreGrpcConnectionBackoffInitial = 5 * time.Second
	// CoreGrpcConnectionBackoffMax is the maximum delay between connection retries.
	CoreGrpcConnectionBackoffMax = 60 * time.Second
)

// CoreGrpcClientConfig is the data structure for the client configuration
type CoreGrpcClientConfig struct {
	// The address of the server <host>:<port>
	Address string
	// Secure flag
	Secure SecureOptions
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

// NewCoreGrpcClient creates a new Core gRPC client, this is called by Site Agent startup code and cert reload routine
// Caller is responsible for retrying connection failure
func NewCoreGrpcClient(config *CoreGrpcClientConfig) (client *CoreGrpcClient, err error) {
	// Validate the config
	if config.Address == "" {
		log.Error().Err(ErrCoreGrpcClientInvalidAddress).Msg("CoreGrpcClient: No address provided")
		return nil, ErrCoreGrpcClientInvalidAddress
	}
	client = &CoreGrpcClient{}

	switch config.Secure {
	case InsecureGrpc:
		// No secure options
		// Default option
		// connect with plain TCP
		log.Debug().Msg("CoreGrpcClient: Using insecure gRPC connection. WARNING: This should not be used in Production)")
		client.dialOpts = append(client.dialOpts, grpc.WithTransportCredentials(insecure.NewCredentials()))
	case ServerTLS:
		log.Debug().Msg("CoreGrpcClient: Using server TLS connection")
		// Validate the config contains server ca path
		if config.ServerCAPath == "" {
			log.Error().Err(ErrCoreGrpcClientInvalidServerCA).Msg("CoreGrpcClient: No server CA path provided")
			return nil, ErrCoreGrpcClientInvalidServerCA
		}
		if config.SkipServerAuth {
			// Server TLS
			// connect with TLS but not mutual TLS
			log.Info().Msg("CoreGrpcClient: Skipping server auth in TLS. WARNING: This should not be used in Production)")
			tlsConfig := &tls.Config{
				InsecureSkipVerify: true,
			}
			// Load the server ca
			_, err := credentials.NewClientTLSFromFile(config.ServerCAPath, "")
			if err != nil {
				log.Error().Err(err).Msg("CoreGrpcClient: Failed to load server CA cert")
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
				log.Error().Err(err).Msg("CoreGrpcClient: Failed to load server CA cert")
				return nil, err
			}
			// Append the dial option
			client.dialOpts = append(client.dialOpts, grpc.WithTransportCredentials(creds))
		}
	case MutualTLS:
		// Mutual TLS
		// connect with mutual TLS
		log.Debug().Msg("CoreGrpcClient: Using mutual TLS connection")
		// 1. Load the client certificates
		clientCert, err := tls.LoadX509KeyPair(config.ClientCertPath, config.ClientKeyPath)
		if err != nil {
			log.Error().Err(err).Msg("CoreGrpcClient: Failed to load client cert")
			return nil, err
		}
		// 2. Load the Trust chain, root ca
		cabytes, err := os.ReadFile(config.ServerCAPath)
		if err != nil {
			log.Error().Err(err).Msg("CoreGrpcClient: Failed to load Root CA cert")

			return nil, err
		}
		capool := x509.NewCertPool()
		if !capool.AppendCertsFromPEM(cabytes) {
			return nil, fmt.Errorf("CoreGrpcClient: Failed to append CA cert to CA pool")
		}
		mutualTLSConfig := &tls.Config{
			Certificates: []tls.Certificate{clientCert},
			RootCAs:      capool,
		}
		creds := credentials.NewTLS(mutualTLSConfig)

		// Append to the dial option
		client.dialOpts = append(client.dialOpts, grpc.WithTransportCredentials(creds))

	default:
		log.Error().Err(ErrCoreGrpcClientInvalidSecureOpts).Msg("CoreGrpcClient: Invalid dial options")
		return nil, ErrCoreGrpcClientInvalidSecureOpts
	}

	client.dialOpts = append(client.dialOpts, grpc.WithDefaultCallOptions(grpc.MaxCallRecvMsgSize(coreGrpcMaxRecvMsgSize)))

	// configure interceptors
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
		log.Error().Err(err).Msg("CoreGrpcClient: Failed to initialize gRPC client")
		return nil, err
	}
	log.Info().Msg("CoreGrpcClient: gRPC client initialized")

	// Create Core gRPC service client
	client.grpcServiceClient = corev1.NewForgeClient(client.conn)
	log.Info().Msg("CoreGrpcClient: Client created")

	// Check the version of the server
	ctx, cancel := context.WithDeadline(context.Background(), time.Now().Add(time.Duration(defaultCoreGrpcDialTimeoutSeconds)*time.Second))
	defer cancel()
	_, err = client.grpcServiceClient.Version(ctx, &corev1.VersionRequest{})
	if err != nil {
		log.Error().Err(err).Msg("CoreGrpcClient: Failed to get version from server")
		return nil, fmt.Errorf("CoreGrpcClient: Failed to get version from server: %w", err)
	}

	log.Info().Msg("CoreGrpcClient: Successfully connected to server")

	return client, nil
}

// CoreGrpcClient is the data structure for the client
type CoreGrpcClient struct {
	// The client connection
	conn *grpc.ClientConn
	// gRPC dial options
	dialOpts []grpc.DialOption
	// gRPC service client interface
	grpcServiceClient corev1.ForgeClient
}

// Close gracefully shuts down the client's gRPC connection.
func (cc *CoreGrpcClient) Close() error {
	if cc.conn != nil {
		// Close the grpc.ClientConn.
		return cc.conn.Close()
	}
	return nil
}

// GrpcServiceClient client getter
func (client *CoreGrpcClient) GrpcServiceClient() corev1.ForgeClient {
	return client.grpcServiceClient
}

// CoreGrpcAtomicClient is an atomic wrapper around the CoreGrpcClient
type CoreGrpcAtomicClient struct {
	Config  *CoreGrpcClientConfig
	value   *atomic.Value
	version atomic.Int64
}

// Version returns the current version of the CoreGrpcClient
func (cac *CoreGrpcAtomicClient) Version() int64 {
	return cac.version.Load()
}

// SwapClient atomically replaces the current CoreGrpcClient with a new one,
// returning the old client for the caller to manage.
func (cac *CoreGrpcAtomicClient) SwapClient(newClient *CoreGrpcClient) *CoreGrpcClient {

	// Atomically replace the current client with the new one and return the old client.
	oldClientInterface := cac.value.Swap(newClient)

	// Type assert the returned value to *CoreGrpcClient.
	// This should always succeed if the correct type was stored initially.
	oldClient, ok := oldClientInterface.(*CoreGrpcClient)
	if !ok {
		log.Error().Msg("SwapClient: Type assertion failed for the old client")
		return nil
	}

	// Increment the version number
	cac.version.Add(1)

	return oldClient
}

// GetClient returns the current version of CoreGrpcClient from the atomic value.
// Returns nil if the client has not been initialized yet.
func (cac *CoreGrpcAtomicClient) GetClient() *CoreGrpcClient {
	v := cac.value.Load()
	if v == nil {
		return nil
	}
	client, _ := v.(*CoreGrpcClient)

	return client
}

// CheckAndReloadCerts continuously monitors the TLS certificates for changes.
// If a change is detected, it reinitializes the CoreGrpcClient with the new certificates to ensure secure communication.
func (cac *CoreGrpcAtomicClient) CheckAndReloadCerts(initialClientCertMD5, initialServerCAMD5 []byte) {
	// Initialize contextual logger
	logger := log.With().Str("Component", "CoreGrpc").Str("Operation", "CheckAndReloadCerts").Logger()

	ticker := time.NewTicker(getCertificateCheckInterval())
	defer ticker.Stop()

	lastClientCertMD5, lastServerCAMD5 := initialClientCertMD5, initialServerCAMD5
	var (
		reloadStarted time.Time
		reloadBackoff time.Duration
	)

	for range ticker.C {
		changed, newClientMD5, newServerMD5, err := cac.CheckCertificates(lastClientCertMD5, lastServerCAMD5)
		if err != nil {
			logger.Error().Err(err).Msg("Error checking certificates for changes")
			continue
		}

		if !changed && reloadBackoff == 0 {
			continue
		}

		if reloadBackoff == 0 {
			reloadStarted = time.Now()
			reloadBackoff = CoreGrpcConnectionBackoffInitial
		} else if time.Since(reloadStarted) >= CoreGrpcConnectionRetryTimeout {
			panic(fmt.Errorf("Core gRPC: failed to reinitialize gRPC client with new certificates within %s",
				CoreGrpcConnectionRetryTimeout))
		}

		newClient, err := NewCoreGrpcClient(cac.Config)
		if err != nil {
			if time.Since(reloadStarted) >= CoreGrpcConnectionRetryTimeout {
				panic(fmt.Errorf("Core gRPC: failed to reinitialize gRPC client with new certificates within %s: %w",
					CoreGrpcConnectionRetryTimeout, err))
			}
			logger.Error().Err(err).Dur("RetryIn", reloadBackoff).Msg("Failed to reinitialize gRPC client with new certificates, retrying")
			ticker.Reset(reloadBackoff)
			reloadBackoff *= 2
			if reloadBackoff > CoreGrpcConnectionBackoffMax {
				reloadBackoff = CoreGrpcConnectionBackoffMax
			}
			continue
		}

		reloadBackoff = 0

		// Atomically update the client instance and get the old one.
		oldClient := cac.SwapClient(newClient)

		// Delayed closure of the old client.
		go func(clientToClose *CoreGrpcClient) {
			// Delay the closure to allow ongoing client requests to complete.
			time.Sleep(10 * time.Second) // Adjust the delay as needed.

			// Ensure the client exists and has a connection to close.
			if clientToClose != nil {
				if err := clientToClose.Close(); err != nil {
					log.Error().Err(err).Msg("Error closing old CoreGrpcClient connection")
				}
			}
		}(oldClient)

		logger.Info().Msg("gRPC client successfully reinitialized with new certificates")

		// Update the stored MD5 hashes with the new ones for the next comparison.
		lastClientCertMD5, lastServerCAMD5 = newClientMD5, newServerMD5

		// Reset the ticker interval to the default
		ticker.Reset(getCertificateCheckInterval())
	}
}

// GetInitialCertMD5 retrieves the MD5 hash of the initial set of certificate that the client is Using
func (cac *CoreGrpcAtomicClient) GetInitialCertMD5() (clientCertMD5, serverCAMD5 []byte, err error) {
	// Load and hash the client certificate
	clientCertBytes, err := os.ReadFile(cac.Config.ClientCertPath)
	if err != nil {
		return nil, nil, err
	}
	clientCertMD5Hash := md5.Sum(clientCertBytes)
	clientCertMD5 = clientCertMD5Hash[:]

	// Load and hash the server CA certificate using os.ReadFile
	serverCABytes, err := os.ReadFile(cac.Config.ServerCAPath)
	if err != nil {
		return nil, nil, err
	}
	serverCAMD5Hash := md5.Sum(serverCABytes)
	serverCAMD5 = serverCAMD5Hash[:]

	return clientCertMD5, serverCAMD5, nil
}

// CheckCertificates checks if the client and server CA certificates have changed
func (cac *CoreGrpcAtomicClient) CheckCertificates(lastClientCertMD5, lastServerCAMD5 []byte) (bool, []byte, []byte, error) {
	// Load and hash the client certificate using os.ReadFile
	clientCertBytes, err := os.ReadFile(cac.Config.ClientCertPath)
	if err != nil {
		return false, lastClientCertMD5, lastServerCAMD5, err
	}
	clientCertMD5 := md5.Sum(clientCertBytes)

	// Load and hash the server CA certificate using os.ReadFile
	serverCABytes, err := os.ReadFile(cac.Config.ServerCAPath)
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

// NewCoreGrpcAtomicClient creates a new CoreGrpcAtomicClient
func NewCoreGrpcAtomicClient(config *CoreGrpcClientConfig) *CoreGrpcAtomicClient {
	// Create the atomic value
	atomicClient := &CoreGrpcAtomicClient{
		Config:  config,
		value:   &atomic.Value{},
		version: atomic.Int64{},
	}

	return atomicClient
}

func getCertificateCheckInterval() time.Duration {
	value, ok := os.LookupEnv("CORE_GRPC_CERT_CHECK_INTERVAL")
	if ok {
		interval, err := strconv.Atoi(value)
		if err == nil {
			return time.Duration(interval) * time.Second
		}
		log.Error().Err(err).Str("CORE_GRPC_CERT_CHECK_INTERVAL", value).Msg("Invalid value specified for Core gRPC certificate check interval")
	}
	log.Warn().Msg("Using default Core gRPC certificate check interval")
	return defaultCheckCertificateIntervalSeconds * time.Second
}

func equalMD5(a, b []byte) bool {
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func SliceToChunks[T any](slice []T, chunkSize int) (chunks [][]T) {
	chunks = [][]T{}
	for {
		if len(slice) == 0 {
			break
		}

		if len(slice) < chunkSize {
			chunkSize = len(slice)
		}

		chunks = append(chunks, slice[0:chunkSize])
		slice = slice[chunkSize:]
	}

	return chunks
}
