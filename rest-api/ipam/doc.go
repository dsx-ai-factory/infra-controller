/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

/*
Package ipam is a ip address management library for ip's and prefixes (networks).

It uses either memory or postgresql database to store the ip's and prefixes.
You can also bring you own Storage implementation as you need.

Example usage:

	import (
		"fmt"
		goipam "github.com/metal-stack/go-ipam"
	)


	func main() {
		// create a ipamer with in memory storage
		ipam := goipam.New()

		prefix, err := ipam.NewPrefix("192.168.0.0/24")
		if err != nil {
			panic(err)
		}

		ip, err := ipam.AcquireIP(prefix)
		if err != nil {
			panic(err)
		}
		fmt.Printf("got IP: %s", ip.IP)

		err = ipam.ReleaseIP(ip)
		if err != nil {
			panic(err)
		}
		fmt.Printf("IP: %s released.", ip.IP)
	}
*/
package ipam
