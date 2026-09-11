/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"strings"
	"sync"

	redisotel "github.com/redis/go-redis/extra/redisotel/v9"
	redigo "github.com/redis/go-redis/v9"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

// RedisConfig holds connection parameters for a Redis-backed IPAM storage.
type RedisConfig struct {
	IP        string
	Port      string
	Username  string
	Password  string
	TLSConfig *tls.Config
}

const namespaceKey = "namespaces"

type redis struct {
	rdb        *redigo.Client
	namespaces map[string]struct{}
	lock       sync.RWMutex
}

// NewRedis create a redis storage for ipam
func NewRedis(ctx context.Context, ip, port string) (Storage, error) {
	return NewRedisFromConfig(ctx, RedisConfig{IP: ip, Port: port})
}

// NewRedisFromConfig creates a redis storage with full connection options
// including optional authentication and TLS.
func NewRedisFromConfig(ctx context.Context, cfg RedisConfig) (Storage, error) {
	return newRedisFromConfig(ctx, cfg)
}

func (r *redis) Name() string {
	return "redis"
}

func newRedisFromConfig(ctx context.Context, cfg RedisConfig) (*redis, error) {
	opts := &redigo.Options{
		Addr:      fmt.Sprintf("%s:%s", cfg.IP, cfg.Port),
		Username:  cfg.Username,
		Password:  cfg.Password,
		DB:        0,
		TLSConfig: cfg.TLSConfig,
	}
	rdb := redigo.NewClient(opts)

	if cotel.Enabled() {
		if terr := redisotel.InstrumentTracing(rdb); terr != nil {
			return nil, fmt.Errorf("failed to instrument redis tracing: %w", terr)
		}
	}

	r := &redis{
		rdb:        rdb,
		namespaces: make(map[string]struct{}),
		lock:       sync.RWMutex{},
	}
	if err := r.CreateNamespace(ctx, defaultNamespace); err != nil {
		return nil, err
	}
	return r, nil
}

func (r *redis) checkNamespaceExists(ctx context.Context, namespace string) error {
	if namespace == "" {
		namespace = defaultNamespace
	}

	if _, ok := r.namespaces[namespace]; ok {
		return nil
	}
	found, err := r.rdb.SIsMember(ctx, namespaceKey, namespace).Result()
	if err != nil {
		return fmt.Errorf("error checking namespace: %w", err)
	}
	if !found {
		return ErrNamespaceDoesNotExist
	}
	r.namespaces[namespace] = struct{}{}
	return nil
}

func (r *redis) CreatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	if namespace != prefix.Namespace {
		return Prefix{}, fmt.Errorf("unable to update prefix:%s, namespace mismatch:%s != %s", prefix.Cidr, prefix.Namespace, namespace)
	}

	if namespace == "" {
		namespace = defaultNamespace
	}

	r.CreateNamespace(ctx, namespace)

	r.lock.Lock()
	defer r.lock.Unlock()

	key := prefix.Cidr + "@" + namespace

	existing, err := r.rdb.Exists(ctx, key).Result()
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to read existing prefix:%v, error:%w", prefix, err)
	}
	if existing != 0 {
		return Prefix{}, fmt.Errorf("prefix:%v already exists", prefix)
	}
	pfx, err := prefix.toJSON()
	if err != nil {
		return Prefix{}, err
	}
	err = r.rdb.Set(ctx, key, pfx, 0).Err()
	return prefix, err
}

func (r *redis) ReadPrefix(ctx context.Context, prefix, namespace string) (Prefix, error) {
	r.lock.RLock()
	defer r.lock.RUnlock()

	if namespace == "" {
		namespace = defaultNamespace
	}

	key := prefix + "@" + namespace
	result, err := r.rdb.Get(ctx, key).Result()
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to read existing prefix:%v, error:%w", prefix, err)
	}
	return fromJSON([]byte(result))
}

func (r *redis) DeleteAllPrefixes(ctx context.Context, namespace string) error {
	r.lock.RLock()
	defer r.lock.RUnlock()

	if namespace == "" {
		namespace = defaultNamespace
	}

	if err := r.checkNamespaceExists(ctx, namespace); err != nil {
		return err
	}

	pfxs, err := r.rdb.Keys(ctx, "*@"+namespace).Result()
	if err != nil {
		return fmt.Errorf("unable to get all prefix cidrs:%w", err)
	}

	for _, pfx := range pfxs {
		_, err := r.rdb.Del(ctx, pfx).Result()
		if err != nil {
			return err
		}
	}

	return nil
}

func (r *redis) ReadAllPrefixes(ctx context.Context, namespace string) (Prefixes, error) {
	r.lock.RLock()
	defer r.lock.RUnlock()

	if namespace == "" {
		namespace = defaultNamespace
	}

	pfxs, err := r.rdb.Keys(ctx, "*@"+namespace).Result()
	if err != nil {
		return nil, fmt.Errorf("unable to get all prefix cidrs:%w", err)
	}

	result := []Prefix{}
	for _, pfx := range pfxs {
		v, err := r.rdb.Get(ctx, pfx).Bytes()
		if err != nil {
			return nil, err
		}
		pfx, err := fromJSON(v)
		if err != nil {
			return nil, err
		}
		result = append(result, pfx)
	}
	return result, nil
}

func (r *redis) ReadAllPrefixCidrs(ctx context.Context, namespace string) ([]string, error) {
	r.lock.RLock()
	defer r.lock.RUnlock()

	if namespace == "" {
		namespace = defaultNamespace
	}

	pfxs, err := r.rdb.Keys(ctx, "*@"+namespace).Result()
	if err != nil {
		return nil, fmt.Errorf("unable to get all prefix cidrs:%w", err)
	}
	ps := make([]string, 0, len(pfxs))
	for _, cidr := range pfxs {
		if strings.HasSuffix(cidr, "@"+namespace) {
			c := strings.TrimSuffix(cidr, "@"+namespace)
			ps = append(ps, c)
		}
	}
	return ps, nil
}

func (r *redis) UpdatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	r.lock.Lock()
	defer r.lock.Unlock()

	if namespace != prefix.Namespace {
		return Prefix{}, fmt.Errorf("unable to update prefix:%s, namespace mismatch:%s != %s", prefix.Cidr, prefix.Namespace, namespace)
	}

	if namespace == "" {
		namespace = defaultNamespace
	}

	if err := r.checkNamespaceExists(ctx, namespace); err != nil {
		return Prefix{}, err
	}

	oldVersion := prefix.version
	prefix.version = oldVersion + 1
	pn, err := prefix.toJSON()
	if err != nil {
		return Prefix{}, err
	}

	key := prefix.Cidr + "@" + namespace

	txf := func(tx *redigo.Tx) error {
		// Get current value or zero.
		p, err := tx.Get(ctx, key).Result()
		if err != nil && !errors.Is(err, redigo.Nil) {
			return err
		}
		oldPrefix, err := fromJSON([]byte(p))
		if err != nil {
			return err
		}
		// Actual operation (local in optimistic lock).
		if oldPrefix.version != oldVersion {
			return fmt.Errorf("%w: unable to update prefix:%s", ErrOptimisticLockError, key)
		}

		// Operation is committed only if the watched keys remain unchanged.
		_, err = tx.TxPipelined(ctx, func(pipe redigo.Pipeliner) error {
			pipe.Set(ctx, key, pn, 0)
			return nil
		})
		return err
	}
	err = r.rdb.Watch(ctx, txf, key)
	if err != nil {
		return Prefix{}, err
	}

	return prefix, nil
}
func (r *redis) DeletePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	r.lock.Lock()
	defer r.lock.Unlock()

	if namespace != prefix.Namespace {
		return Prefix{}, fmt.Errorf("unable to delete prefix:%s, namespace mismatch:%s != %s", prefix.Cidr, prefix.Namespace, namespace)
	}

	if namespace == "" {
		namespace = defaultNamespace
	}

	key := prefix.Cidr + "@" + namespace

	_, err := r.rdb.Del(ctx, key).Result()
	if err != nil {
		return *prefix.deepCopy(), err
	}
	return *prefix.deepCopy(), nil
}

func (r *redis) CreateNamespace(ctx context.Context, namespace string) error {
	r.lock.Lock()
	defer r.lock.Unlock()

	if namespace == "" {
		namespace = defaultNamespace
	}

	if _, ok := r.namespaces[namespace]; ok {
		return nil
	}
	if err := r.rdb.SAdd(ctx, namespaceKey, namespace).Err(); err != nil {
		return err
	}
	r.namespaces[namespace] = struct{}{}

	return nil
}

func (r *redis) ListNamespaces(ctx context.Context) ([]string, error) {
	r.lock.Lock()
	defer r.lock.Unlock()
	return r.rdb.SMembers(ctx, namespaceKey).Result()
}

func (r *redis) DeleteNamespace(ctx context.Context, namespace string) error {
	if err := r.DeleteAllPrefixes(ctx, namespace); err != nil {
		return err
	}
	r.lock.Lock()
	defer r.lock.Unlock()
	if err := r.rdb.SRem(ctx, namespaceKey, namespace).Err(); err != nil {
		return err
	}
	delete(r.namespaces, namespace)
	return nil
}
