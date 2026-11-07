/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package main

import (
	"context"
	"fmt"
	"net/http"
	"os"

	"connectrpc.com/connect"
	v1 "github.com/NVIDIA/infra-controller/rest-api/ipam/api/v1"
	"github.com/NVIDIA/infra-controller/rest-api/ipam/api/v1/apiv1connect"
	compress "github.com/klauspost/connect-compress/v2"
	"github.com/metal-stack/v"
	"github.com/urfave/cli/v2"
)

func main() {

	app := &cli.App{
		Name:    "cli",
		Usage:   "cli for go-ipam",
		Version: v.V.String(),
		Flags: []cli.Flag{
			&cli.StringFlag{
				Name:    "grpc-server-endpoint",
				Value:   "http://localhost:9090",
				Usage:   "gRPC server endpoint",
				EnvVars: []string{"GOIPAM_GRPC_SERVER_ENDPOINT"},
			},
		},
		Commands: []*cli.Command{
			{
				Name:    "prefix",
				Aliases: []string{"p"},
				Usage:   "prefix manipulation",
				Subcommands: []*cli.Command{
					{
						Name:  "create",
						Usage: "create a prefix",
						Flags: []cli.Flag{
							&cli.StringFlag{
								Name: "cidr",
							},
						},
						Action: func(ctx *cli.Context) error {
							c := client(ctx)
							result, err := c.CreatePrefix(context.Background(), connect.NewRequest(&v1.CreatePrefixRequest{
								Cidr: ctx.String("cidr"),
							}))

							if err != nil {
								return err
							}
							fmt.Printf("prefix:%q created\n", result.Msg.Prefix.Cidr)
							return nil
						},
					},
					{
						Name:  "acquire",
						Usage: "acquire a child prefix",
						Flags: []cli.Flag{
							&cli.StringFlag{
								Name: "parent",
							},
							&cli.UintFlag{
								Name: "length",
							},
						},
						Action: func(ctx *cli.Context) error {
							c := client(ctx)
							result, err := c.AcquireChildPrefix(context.Background(), connect.NewRequest(&v1.AcquireChildPrefixRequest{
								Cidr:   ctx.String("parent"),
								Length: uint32(ctx.Uint("length")),
							}))

							if err != nil {
								return err
							}
							fmt.Printf("child prefix:%q from %q created\n", result.Msg.Prefix.Cidr, result.Msg.Prefix.ParentCidr)
							return nil
						},
					},
					{
						Name:  "release",
						Usage: "release a child prefix",
						Flags: []cli.Flag{
							&cli.StringFlag{
								Name: "cidr",
							},
						},
						Action: func(ctx *cli.Context) error {
							c := client(ctx)
							result, err := c.ReleaseChildPrefix(context.Background(), connect.NewRequest(&v1.ReleaseChildPrefixRequest{
								Cidr: ctx.String("cidr"),
							}))

							if err != nil {
								return err
							}
							if result.Msg == nil || result.Msg.Prefix == nil {
								return fmt.Errorf("result contains no prefix")
							}
							fmt.Printf("child prefix:%q from %q released\n", result.Msg.Prefix.Cidr, result.Msg.Prefix.ParentCidr)
							return nil
						},
					},
					{
						Name:  "list",
						Usage: "list all prefixes",
						Action: func(ctx *cli.Context) error {
							c := client(ctx)
							result, err := c.ListPrefixes(context.Background(), connect.NewRequest(&v1.ListPrefixesRequest{}))

							if err != nil {
								return err
							}
							for _, p := range result.Msg.Prefixes {
								fmt.Printf("Prefix:%q parent:%q\n", p.Cidr, p.ParentCidr)
							}
							return nil
						},
					},
					{
						Name:  "delete",
						Usage: "delete a prefix",
						Flags: []cli.Flag{
							&cli.StringFlag{
								Name: "cidr",
							},
						},
						Action: func(ctx *cli.Context) error {
							c := client(ctx)
							result, err := c.DeletePrefix(context.Background(), connect.NewRequest(&v1.DeletePrefixRequest{
								Cidr: ctx.String("cidr"),
							}))

							if err != nil {
								return err
							}
							fmt.Printf("prefix:%q deleted\n", result.Msg.Prefix.Cidr)
							return nil
						},
					},
				},
			},
			{
				Name:    "ip",
				Aliases: []string{"i"},
				Usage:   "ip manipulation",
				Subcommands: []*cli.Command{
					{
						Name:  "acquire",
						Usage: "acquire a ip",
						Flags: []cli.Flag{
							&cli.StringFlag{
								Name: "prefix",
							},
						},
						Action: func(ctx *cli.Context) error {
							c := client(ctx)
							result, err := c.AcquireIP(context.Background(), connect.NewRequest(&v1.AcquireIPRequest{
								PrefixCidr: ctx.String("prefix"),
							}))

							if err != nil {
								return err
							}
							fmt.Printf("ip:%q acquired\n", result.Msg.Ip.Ip)
							return nil
						},
					},
					{
						Name:  "release",
						Usage: "release a ip",
						Flags: []cli.Flag{
							&cli.StringFlag{
								Name: "ip",
							},
							&cli.StringFlag{
								Name: "prefix",
							},
						},
						Action: func(ctx *cli.Context) error {
							c := client(ctx)
							result, err := c.ReleaseIP(context.Background(), connect.NewRequest(&v1.ReleaseIPRequest{
								Ip:         ctx.String("ip"),
								PrefixCidr: ctx.String("prefix"),
							}))

							if err != nil {
								return err
							}
							fmt.Printf("ip:%q released\n", result.Msg.Ip.Ip)
							return nil
						},
					},
				},
			},
			{
				Name:  "backup",
				Usage: "create and restore a backup",
				Subcommands: []*cli.Command{
					{
						Name:  "create",
						Usage: "create a json file of the whole ipam db for backup purpose",
						Action: func(ctx *cli.Context) error {
							c := client(ctx)
							result, err := c.Dump(context.Background(), connect.NewRequest(&v1.DumpRequest{}))
							if err != nil {
								return err
							}
							fmt.Println(result.Msg.Dump)
							return nil
						},
					},
					{
						Name:  "restore",
						Usage: "load the whole ipam db from json file, previously created, only works if database is already empty",
						Flags: []cli.Flag{
							&cli.StringFlag{
								Name: "file",
							},
						},
						Action: func(ctx *cli.Context) error {
							c := client(ctx)
							json, err := os.ReadFile(ctx.String("file"))
							if err != nil {
								return err
							}
							_, err = c.Load(context.Background(), connect.NewRequest(&v1.LoadRequest{
								Dump: string(json),
							}))

							if err != nil {
								return err
							}
							fmt.Printf("database restored\n")
							return nil
						},
					},
				},
			},
		},
	}
	err := app.Run(os.Args)
	if err != nil {
		fmt.Println(err.Error())
		os.Exit(1)
	}
}

func client(ctx *cli.Context) apiv1connect.IpamServiceClient {

	return apiv1connect.NewIpamServiceClient(
		http.DefaultClient,
		ctx.String("grpc-server-endpoint"),
		connect.WithGRPC(),
		compress.WithAll(compress.LevelBalanced),
	)
}
