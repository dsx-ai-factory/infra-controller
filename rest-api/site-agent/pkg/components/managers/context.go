// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package managers implements some core utilities
package managers

import (
	"context"
	"math/rand"
	"os"
	"os/signal"
	"path/filepath"
	"sync"
	"syscall"
	"time"

	"github.com/rs/zerolog"
)

// NewDefaultContext provides a default context for applications.
func NewDefaultContext(parent context.Context) context.Context {
	ctx := WithAppName(parent, DefaultAppName())
	ctx = WithDefaultLogger(ctx)
	ctx = WithSignalHandler(ctx)
	ctx = WithRandomSeed(ctx, time.Now().UnixNano())
	return ctx
}

type key int

const (
	appNameKey key = iota
	loggerKey
	randKey
	clockKey
)

// DefaultAppName gets the name of the current executable
func DefaultAppName() string {
	exePath, err := os.Executable()
	if err != nil {
		return ""
	}
	return filepath.Base(exePath)
}

// WithAppName returns a context with the specified app name
func WithAppName(parent context.Context, appName string) context.Context {
	return context.WithValue(parent, appNameKey, appName)
}

// GetAppName fetches the appname key value
func GetAppName(ctx context.Context) string {
	if appName, ok := ctx.Value(appNameKey).(string); ok {
		return appName
	}
	return ""
}

// WithDefaultLogger returns a context with the default logger
func WithDefaultLogger(parent context.Context) context.Context {
	log := zerolog.New(os.Stdout).With().Logger()
	return context.WithValue(parent, loggerKey, log.WithContext(parent))
}

// Following setup are from k8s.io/sample-controller/pkg/signals

var onlyOneSignalHandler = make(chan struct{})
var shutdownSignals = []os.Signal{os.Interrupt, syscall.SIGTERM}

// WithSignalHandler returns a context with signal handlers
func WithSignalHandler(parent context.Context) context.Context {
	// Panics if called twice
	close(onlyOneSignalHandler)

	ctx, cancel := context.WithCancel(parent)
	c := make(chan os.Signal, 2)
	signal.Notify(c, shutdownSignals...)
	go func() {
		s := <-c
		ManagerAccess.Data.EB.Log.Info().Msgf("Received os.Signal %v, shutting down ...", s)
		cancel()

		s = <-c
		ManagerAccess.Data.EB.Log.Info().Msgf("Received second os.Signal %v, exit immediately !", s)
		os.Exit(1)
	}()

	return ctx
}

// Rand wraps *rand.Rand, provides thread-safe access to a subset of
// methods.
type Rand struct {
	sync.Mutex
	*rand.Rand
}

// Float64 returns a random float64
func (r *Rand) Float64() float64 {
	r.Lock()
	defer r.Unlock()
	return r.Rand.Float64()
}

// Intn returns a random int
func (r *Rand) Intn(n int) int {
	r.Lock()
	defer r.Unlock()
	return r.Rand.Intn(n)
}

// RandomBytes returns n random bytes.
func (r *Rand) RandomBytes(n int) []byte {
	b := make([]byte, n)
	r.Lock()
	defer r.Unlock()
	r.Read(b)
	return b
}

var defaultRand = &Rand{Rand: rand.New(rand.NewSource(0))}

// WithRandomSeed sets a seed
func WithRandomSeed(parent context.Context, seed int64) context.Context {
	r := &Rand{Rand: rand.New(rand.NewSource(seed))}
	return context.WithValue(parent, randKey, r)
}

// GetRandFloat64 returns a random float64
func GetRandFloat64(ctx context.Context) float64 {
	if r, ok := ctx.Value(randKey).(*Rand); ok {
		return r.Float64()
	}
	return defaultRand.Float64()
}

// GetRandIntn returns a random int
func GetRandIntn(ctx context.Context, n int) int {
	if r, ok := ctx.Value(randKey).(*Rand); ok {
		return r.Intn(n)
	}
	return defaultRand.Intn(n)
}

// GetRandomBytes returns n random bytes
func GetRandomBytes(ctx context.Context, n int) []byte {
	if r, ok := ctx.Value(randKey).(*Rand); ok {
		return r.RandomBytes(n)
	}
	return defaultRand.RandomBytes(n)
}

var defaultClock = time.Now

// WithClock gets a context with a clock func
func WithClock(parent context.Context, clockFunc func() time.Time) context.Context {
	return context.WithValue(parent, clockKey, clockFunc)
}

// GetCurrentTime returns the current time from the context or default
func GetCurrentTime(ctx context.Context) time.Time {
	if clock, ok := ctx.Value(clockKey).(func() time.Time); ok {
		return clock()
	}
	return defaultClock()
}

// ContainsString is a helper function
func ContainsString(slice []string, s string) bool {
	for _, item := range slice {
		if item == s {
			return true
		}
	}
	return false
}

// RemoveString is a helper function
func RemoveString(slice []string, s string) (result []string) {
	for _, item := range slice {
		if item == s {
			continue
		}
		result = append(result, item)
	}
	return
}
