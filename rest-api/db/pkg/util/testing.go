// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"context"
	"fmt"
	"os"
	"strconv"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/uptrace/bun/extra/bundebug"

	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
)

// TestDBConfig describes a test DB config params
type TestDBConfig struct {
	Host     string
	Port     int
	Name     string
	User     string
	Password string
}

// getTestDBParams returns the DB params for a test DB
func getTestDBParams() TestDBConfig {
	tdbcfg := TestDBConfig{
		Host:     "localhost",
		Port:     30432,
		Name:     "nicotest",
		User:     "postgres",
		Password: "postgres",
	}

	port, ok := os.LookupEnv("PGPORT")
	if ok {
		portv, err := strconv.Atoi(port)
		if err == nil {
			tdbcfg.Port = portv
		}
	}

	user, ok := os.LookupEnv("PGUSER")
	if ok {
		tdbcfg.User = user
	}

	password, ok := os.LookupEnv("PGPASSWORD")
	if ok {
		tdbcfg.Password = password
	}

	if os.Getenv("CI") == "true" {
		tdbcfg.Host = "postgres"
		tdbcfg.Port = 5432
	}

	return tdbcfg
}

// GetTestIpamDB returns a test IPAM DB
func GetTestIpamDB(t *testing.T) cipam.Storage {
	tdbcfg := getTestDBParams()

	ipamDB, err := cipam.NewPostgresStorage(tdbcfg.Host, fmt.Sprintf("%d", tdbcfg.Port), tdbcfg.User, tdbcfg.Password, tdbcfg.Name, cipam.SSLModeDisable)
	if err != nil {
		t.Fatal(err)
	}

	return ipamDB
}

// GetTestDBSession returns a test DB session
func GetTestDBSession(t *testing.T, reset bool) *db.Session {
	// Create test DB
	tdbcfg := getTestDBParams()

	dbSession, err := db.NewSession(context.Background(), tdbcfg.Host, tdbcfg.Port, "postgres", tdbcfg.User, tdbcfg.Password, "")
	if err != nil {
		t.Fatal(err)
	}

	count, err := dbSession.DB.NewSelect().Table("pg_database").Where("datname = ?", tdbcfg.Name).Count(context.Background())
	if err != nil {
		dbSession.Close()
		t.Fatal(err)
	}

	if count > 0 && reset {
		_, err = dbSession.DB.Exec("DROP DATABASE " + tdbcfg.Name)
		if err != nil {
			dbSession.Close()
			t.Fatal(err)
		}
	}

	if count == 0 || reset {
		_, err = dbSession.DB.Exec("CREATE DATABASE " + tdbcfg.Name)
		if err != nil {
			dbSession.Close()
			t.Fatal(err)
		}
	}
	// close this session
	dbSession.Close()

	// Create another session to the nicotest database
	dbSession, err = db.NewSession(context.Background(), tdbcfg.Host, tdbcfg.Port, tdbcfg.Name, tdbcfg.User, tdbcfg.Password, "")
	if err != nil {
		t.Fatal(err)
	}

	_, err = dbSession.DB.Exec("CREATE EXTENSION IF NOT EXISTS pg_trgm")
	if err != nil {
		dbSession.Close()
		t.Fatal(err)
	}

	if testing.Verbose() {
		connCount, err := GetDBConnectionCount(dbSession)
		if err == nil {
			fmt.Printf("connections count = %d\n", connCount)
		}
	}

	return dbSession
}

// GetDBConnectionCount returns the count of rows in the pg_stat_activity table
func GetDBConnectionCount(dbSession *db.Session) (int, error) {
	return dbSession.DB.NewSelect().Table("pg_stat_activity").Count(context.Background())
}

// TestInitDB initializes a test DB session
func TestInitDB(t *testing.T) *db.Session {
	dbSession := GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}
