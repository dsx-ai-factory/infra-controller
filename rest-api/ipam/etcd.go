/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"crypto/tls"
	"fmt"
	"log"
	"strings"
	"sync"
	"time"

	clientv3 "go.etcd.io/etcd/client/v3"
	"go.opentelemetry.io/contrib/instrumentation/google.golang.org/grpc/otelgrpc"
	"google.golang.org/grpc"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

type etcd struct {
	etcdDB     *clientv3.Client
	namespaces map[string]struct{}
	lock       sync.RWMutex
}

// NewEtcd create a etcd storage for ipam
func NewEtcd(ctx context.Context, ip, port string, cert, key []byte, insecureskip bool) (Storage, error) {
	return newEtcd(ctx, ip, port, cert, key, insecureskip)
}

func (e *etcd) Name() string {
	return "etcd"
}

func newEtcd(ctx context.Context, ip, port string, cert, key []byte, insecureskip bool) (*etcd, error) {
	etcdConfig := clientv3.Config{
		Endpoints:   []string{fmt.Sprintf("%s:%s", ip, port)},
		DialTimeout: 5 * time.Second,
		Context:     context.Background(),
	}

	if cotel.Enabled() {
		etcdConfig.DialOptions = append(etcdConfig.DialOptions,
			grpc.WithStatsHandler(otelgrpc.NewClientHandler()))
	}

	if cert != nil && key != nil {
		// SSL
		clientCert, err := tls.X509KeyPair(cert, key)
		if err != nil {
			log.Fatal(err)
		}
		tls := &tls.Config{
			Certificates: []tls.Certificate{clientCert},
			// nolint:gosec
			// #nosec G402
			InsecureSkipVerify: insecureskip,
		}
		etcdConfig.TLS = tls
	}
	cli, err := clientv3.New(etcdConfig)
	if err != nil {
		log.Fatal(err)
	}

	e := &etcd{
		etcdDB:     cli,
		namespaces: make(map[string]struct{}),
		lock:       sync.RWMutex{},
	}

	if err := e.CreateNamespace(ctx, defaultNamespace); err != nil {
		return nil, err
	}

	return e, nil
}

func etcdNamespaceKey(namespace string) string {
	return namespaceKey + "/" + namespace
}

// This should ONLY be called when e.Lock() has been acquired
func (e *etcd) checkNamespaceExists(ctx context.Context, namespace string) error {
	if _, ok := e.namespaces[namespace]; ok {
		return nil
	}
	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	res, err := e.etcdDB.Get(ctx, etcdNamespaceKey(namespace))
	if err != nil {
		return fmt.Errorf("unable to read namespace key: %w", err)
	}
	if res.Count == 0 {
		return ErrNamespaceDoesNotExist
	}
	e.namespaces[namespace] = struct{}{}
	return nil
}

func (e *etcd) CreatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	if namespace != prefix.Namespace {
		return Prefix{}, fmt.Errorf("unable to create prefix:%s, namespace mismatch:%s != %s", prefix.Cidr, prefix.Namespace, namespace)
	}

	if namespace == "" {
		namespace = defaultNamespace
	}

	if err := e.CreateNamespace(ctx, namespace); err != nil {
		return Prefix{}, err
	}

	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	key := namespace + "@" + prefix.Cidr
	get, err := e.etcdDB.Get(ctx, key)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to read existing prefix:%v, error:%w", prefix, err)
	}

	if get.Count != 0 {
		return Prefix{}, fmt.Errorf("prefix already exists:%v", prefix)
	}

	pfx, err := prefix.toJSON()
	if err != nil {
		return Prefix{}, err
	}
	ctx, cancel = context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	_, err = e.etcdDB.Put(ctx, key, string(pfx))
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to create prefix:%v, error:%w", prefix, err)
	}

	return prefix, nil
}

func (e *etcd) ReadPrefix(ctx context.Context, prefix string, namespace string) (Prefix, error) {
	if namespace == "" {
		namespace = defaultNamespace
	}

	e.lock.Lock()
	defer e.lock.Unlock()

	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	key := namespace + "@" + prefix
	get, err := e.etcdDB.Get(ctx, key)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to read data from ETCD error:%w", err)
	}

	if get.Count == 0 {
		return Prefix{}, fmt.Errorf("unable to read existing prefix:%v, error:%w", prefix, err)
	}

	return fromJSON(get.Kvs[0].Value)
}

func (e *etcd) DeleteAllPrefixes(ctx context.Context, namespace string) error {
	if namespace == "" {
		namespace = defaultNamespace
	}

	if err := e.checkNamespaceExists(ctx, namespace); err != nil {
		return err
	}

	e.lock.RLock()
	defer e.lock.RUnlock()

	ctx, cancel := context.WithTimeout(ctx, 50*time.Minute)
	defer cancel()
	defaultOpts := []clientv3.OpOption{clientv3.WithPrefix(), clientv3.WithKeysOnly(), clientv3.WithSerializable()}
	pfxs, err := e.etcdDB.Get(ctx, namespace, defaultOpts...)
	if err != nil {
		return fmt.Errorf("unable to get all prefix cidrs:%w", err)
	}

	for _, pfx := range pfxs.Kvs {
		_, err := e.etcdDB.Delete(ctx, string(pfx.Key))
		if err != nil {
			return fmt.Errorf("unable to delete prefix:%w", err)
		}
	}
	return nil
}

func (e *etcd) ReadAllPrefixes(ctx context.Context, namespace string) (Prefixes, error) {
	if namespace == "" {
		namespace = defaultNamespace
	}

	e.lock.Lock()
	defer e.lock.Unlock()

	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	defaultOpts := []clientv3.OpOption{clientv3.WithPrefix(), clientv3.WithKeysOnly(), clientv3.WithSerializable()}
	pfxs, err := e.etcdDB.Get(ctx, namespace, defaultOpts...)
	if err != nil {
		return nil, fmt.Errorf("unable to get all prefix cidrs:%w", err)
	}

	result := Prefixes{}
	for _, pfx := range pfxs.Kvs {
		v, err := e.etcdDB.Get(ctx, string(pfx.Key))
		if err != nil {
			return nil, err
		}
		pfx, err := fromJSON(v.Kvs[0].Value)
		if err != nil {
			return nil, err
		}
		result = append(result, pfx)
	}
	return result, nil
}

func (e *etcd) ReadAllPrefixCidrs(ctx context.Context, namespace string) ([]string, error) {
	if namespace == "" {
		namespace = defaultNamespace
	}

	e.lock.Lock()
	defer e.lock.Unlock()

	allPrefix := []string{}
	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	defaultOpts := []clientv3.OpOption{clientv3.WithPrefix(), clientv3.WithKeysOnly(), clientv3.WithSerializable()}
	pfxs, err := e.etcdDB.Get(ctx, namespace, defaultOpts...)
	if err != nil {
		return nil, fmt.Errorf("unable to get all prefix cidrs:%w", err)
	}

	for _, pfx := range pfxs.Kvs {
		v, err := e.etcdDB.Get(ctx, string(pfx.Key))
		if err != nil {
			return nil, err
		}
		pfx, err := fromJSON(v.Kvs[0].Value)
		if err != nil {
			return nil, err
		}
		allPrefix = append(allPrefix, string(pfx.Cidr))
	}

	return allPrefix, nil
}
func (e *etcd) UpdatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	if namespace != prefix.Namespace {
		return Prefix{}, fmt.Errorf("unable to update prefix:%s, namespace mismatch:%s != %s", prefix.Cidr, prefix.Namespace, namespace)
	}

	if namespace == "" {
		namespace = defaultNamespace
	}

	if err := e.checkNamespaceExists(ctx, namespace); err != nil {
		return Prefix{}, err
	}

	e.lock.Lock()
	defer e.lock.Unlock()

	oldVersion := prefix.version
	prefix.version = oldVersion + 1
	pn, err := prefix.toJSON()
	if err != nil {
		return Prefix{}, err
	}

	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	key := namespace + "@" + prefix.Cidr
	p, err := e.etcdDB.Get(ctx, key)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to read cidrs from ETCD:%w", err)
	}

	if p.Count == 0 {
		return Prefix{}, fmt.Errorf("unable to get all prefix cidrs:%w", err)
	}

	oldPrefix, err := fromJSON([]byte(p.Kvs[0].Value))
	if err != nil {
		return Prefix{}, err
	}

	// Actual operation (local in optimistic lock).
	if oldPrefix.version != oldVersion {
		return Prefix{}, fmt.Errorf("%w: unable to update prefix:%s", ErrOptimisticLockError, prefix.Cidr)
	}

	// Operation is committed only if the watched keys remain unchanged.
	ctx, cancel = context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	_, err = e.etcdDB.Put(ctx, key, string(pn))
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to update prefix:%s, error:%w", prefix.Cidr, err)
	}

	return prefix, nil
}
func (e *etcd) DeletePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	if namespace == "" {
		namespace = defaultNamespace
	}

	if err := e.checkNamespaceExists(ctx, namespace); err != nil {
		return Prefix{}, err
	}

	e.lock.RLock()
	defer e.lock.RUnlock()

	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	key := namespace + "@" + prefix.Cidr
	_, err := e.etcdDB.Delete(ctx, key)
	if err != nil {
		return *prefix.deepCopy(), err
	}
	return *prefix.deepCopy(), nil
}

func (e *etcd) CreateNamespace(ctx context.Context, namespace string) error {
	e.lock.Lock()
	defer e.lock.Unlock()
	if _, ok := e.namespaces[namespace]; ok {
		return nil
	}
	key := etcdNamespaceKey(namespace)
	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	_, err := e.etcdDB.Put(ctx, key, "")
	e.namespaces[namespace] = struct{}{}
	return err
}

func (e *etcd) ListNamespaces(ctx context.Context) ([]string, error) {
	e.lock.Lock()
	defer e.lock.Unlock()

	key := etcdNamespaceKey("")
	defaultOpts := []clientv3.OpOption{clientv3.WithPrefix(), clientv3.WithKeysOnly(), clientv3.WithSerializable()}
	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	res, err := e.etcdDB.Get(ctx, key, defaultOpts...)
	if err != nil {
		return nil, err
	}
	var result []string
	for _, kv := range res.Kvs {
		result = append(result, strings.TrimPrefix(string(kv.Key), key))
	}
	return result, nil
}

func (e *etcd) DeleteNamespace(ctx context.Context, namespace string) error {
	if err := e.DeleteAllPrefixes(ctx, namespace); err != nil {
		return err
	}
	e.lock.Lock()
	defer e.lock.Unlock()
	_, err := e.etcdDB.Delete(ctx, etcdNamespaceKey(namespace))
	delete(e.namespaces, namespace)
	return err
}
