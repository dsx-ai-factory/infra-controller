/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"errors"
	"fmt"
	"sync"

	"go.mongodb.org/mongo-driver/bson"
	"go.mongodb.org/mongo-driver/mongo"
	"go.mongodb.org/mongo-driver/mongo/options"
)

const dbIndex = `prefix.cidr`
const versionKey = `version`

type MongoConfig struct {
	DatabaseName       string
	CollectionName     string
	MongoClientOptions *options.ClientOptions
}

type mongodb struct {
	c    *mongo.Collection
	lock sync.RWMutex
}

func NewMongo(ctx context.Context, config MongoConfig) (Storage, error) {
	return newMongo(ctx, config)
}

func (m *mongodb) Name() string {
	return "mongodb"
}

func newMongo(ctx context.Context, config MongoConfig) (*mongodb, error) {
	m, err := mongo.NewClient(config.MongoClientOptions)
	if err != nil {
		return nil, err
	}
	err = m.Connect(ctx)
	if err != nil {
		return nil, err
	}

	err = m.Ping(ctx, nil)
	if err != nil {
		return nil, err
	}

	c := m.Database(config.DatabaseName).Collection(config.CollectionName)

	_, err = c.Indexes().CreateMany(ctx, []mongo.IndexModel{{
		Keys:    bson.M{dbIndex: 1},
		Options: options.Index().SetUnique(true),
	}})
	if err != nil {
		return nil, err
	}
	return &mongodb{c, sync.RWMutex{}}, nil
}

func (m *mongodb) CreatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	m.lock.Lock()
	defer m.lock.Unlock()

	f := bson.D{{Key: dbIndex, Value: prefix.Cidr}}
	r := m.c.FindOne(ctx, f)

	// ErrNoDocuments should be returned if the prefix does not exist
	if r.Err() == nil {
		return Prefix{}, fmt.Errorf("prefix already exists:%s", prefix.Cidr)
	} else if r.Err() != nil && !errors.Is(r.Err(), mongo.ErrNoDocuments) { // unrelated to ErrNoDocuments.
		return Prefix{}, fmt.Errorf("unable to insert prefix:%s, error:%w", prefix.Cidr, r.Err())
	} // ErrNoDocuments should pass through this block

	_, err := m.c.InsertOne(ctx, prefix.toPrefixJSON())
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to insert prefix:%s, error:%w", prefix.Cidr, err)
	}

	return prefix, nil
}

func (m *mongodb) ReadPrefix(ctx context.Context, prefix string, namespace string) (Prefix, error) {
	m.lock.Lock()
	defer m.lock.Unlock()

	f := bson.D{{Key: dbIndex, Value: prefix}}
	r := m.c.FindOne(ctx, f)

	// ErrNoDocuments should be returned if the prefix does not exist
	if r.Err() != nil && errors.Is(r.Err(), mongo.ErrNoDocuments) {
		return Prefix{}, fmt.Errorf(`prefix not found:%s, error:%w`, prefix, r.Err())
	} else if r.Err() != nil {
		return Prefix{}, fmt.Errorf(`error while trying to find prefix:%s, error:%w`, prefix, r.Err())
	}

	j := prefixJSON{}
	err := r.Decode(&j)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to read prefix:%w", err)
	}
	return j.toPrefix(), nil
}

func (m *mongodb) DeleteAllPrefixes(ctx context.Context, namespace string) error {
	m.lock.Lock()
	defer m.lock.Unlock()

	f := bson.D{{}} // match all documents
	_, err := m.c.DeleteMany(ctx, f)
	if err != nil {
		return fmt.Errorf(`error deleting all prefixes: %w`, err)
	}
	return nil
}

func (m *mongodb) ReadAllPrefixes(ctx context.Context, namespace string) (Prefixes, error) {
	m.lock.Lock()
	defer m.lock.Unlock()

	f := bson.D{{}} // match all documents
	c, err := m.c.Find(ctx, f)
	if err != nil {
		return nil, fmt.Errorf(`error reading all prefixes: %w`, err)
	}
	var r []prefixJSON
	if err := c.All(ctx, &r); err != nil {
		return nil, fmt.Errorf(`error reading all prefixes: %w`, err)
	}

	var s = make([]Prefix, len(r))
	for i, v := range r {
		s[i] = v.toPrefix()
	}

	return s, nil
}

func (m *mongodb) ReadAllPrefixCidrs(ctx context.Context, namespace string) ([]string, error) {
	p, err := m.ReadAllPrefixes(ctx, namespace)
	if err != nil {
		return nil, err
	}
	var s = make([]string, len(p))
	for i, v := range p {
		s[i] = v.Cidr
	}
	return s, nil
}

func (m *mongodb) UpdatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	m.lock.Lock()
	defer m.lock.Unlock()

	oldVersion := prefix.version
	prefix.version = oldVersion + 1

	f := bson.D{{Key: dbIndex, Value: prefix.Cidr}, {Key: versionKey, Value: oldVersion}}

	o := options.Replace().SetUpsert(false)
	r, err := m.c.ReplaceOne(ctx, f, prefix.toPrefixJSON(), o)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to update prefix:%s, error: %w", prefix.Cidr, err)
	}
	if r.MatchedCount == 0 {
		return Prefix{}, fmt.Errorf("%w: unable to update prefix:%s", ErrOptimisticLockError, prefix.Cidr)
	}
	if r.ModifiedCount == 0 {
		return Prefix{}, fmt.Errorf("%w: update did not effect any document:%s",
			ErrOptimisticLockError, prefix.Cidr)
	}

	return prefix, nil
}

func (m *mongodb) DeletePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	m.lock.Lock()
	defer m.lock.Unlock()

	f := bson.D{{Key: dbIndex, Value: prefix.Cidr}}
	r := m.c.FindOneAndDelete(ctx, f)

	// ErrNoDocuments should be returned if the prefix does not exist
	if r.Err() != nil && errors.Is(r.Err(), mongo.ErrNoDocuments) {
		return Prefix{}, fmt.Errorf(`prefix not found:%s, error:%w`, prefix.Cidr, r.Err())
	} else if r.Err() != nil {
		return Prefix{}, fmt.Errorf(`error while trying to find prefix:%s, error:%w`, prefix.Cidr, r.Err())
	}

	j := prefixJSON{}
	err := r.Decode(&j)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to read prefix:%w", err)
	}
	return j.toPrefix(), nil
}

func (m *mongodb) ReadPrefixes(ctx context.Context, namespace string) ([]Prefix, error) {
	return []Prefix{}, fmt.Errorf("error not implemented")
}

func (m *mongodb) CreateNamespace(_ context.Context, namespace string) error {
	return ErrNotImplemented
}

func (m *mongodb) ListNamespaces(_ context.Context) ([]string, error) {
	return nil, ErrNotImplemented
}

func (m *mongodb) DeleteNamespace(_ context.Context, namespace string) error {
	return ErrNotImplemented
}
