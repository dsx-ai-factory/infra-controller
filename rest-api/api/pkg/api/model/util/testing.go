// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import "math/rand"

var TestCommonCloudInit = `#cloud-config
package_update: true
packages:
  - net-tools
autoinstall:
  version: 1
  identity:
    hostname: qapreview
    password: $6$jCfWFbdxh1lK09sY$pxFnrW/yXewYFmgoaywu3WKhdPQg0e8DR8jvedAV.udXM0.i5M6wr4Up2S7ZCN9kNDmg.s7fmrOaXE6nEyzPb/ # Welcome123
    username: ubuntu
  ntp:
    enabled: true
    ntp_client: chrony # Uses cloud-init default chrony configuration
    servers:
      - 132.163.96.5
  keyboard:
    layout: us
    toggle: null
    variant: ""
  locale: en_US
  network:
    version: 2
    ethernets:
      ens5f0:
        critical: true
        dhcp-identifier: mac
        dhcp4: true
        nameservers:
          addresses: [8.8.8.8]
  ssh:
    allow-pw: true
    authorized-keys: [ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQC5bGg4eGyprn25wNEZ2bSiNYvYfLPcvRUNNI2CDx22YNG6+IXt9X+Y4CiTJ14k9QGFnx+vAzNGoLBgvtiwLhQrxOadSypH9KZWTadYK+yR8EWOEAOfgnCf++Vrh2eEKZAayPvWEO6xVgZod+GZtU69JZLv2aO1hMiBPGB4bP4ieFO0oKVM7dZenYg/I7uTppr5NKPkj/xIT7eB+tR3TeSZ44WaK1gZOgh7GL72CCKI5BuuAePXDYZFt0+rPX+kwViDBxzVRDbDwEgFg7NGJFgqt8dPlflw5w6MANvKSJ69yBdxVoJdtANc7mNm4h8RVMT/a1bK0uXADT18hJ2B4YVJRPHposeCM5XtWKXww4yTqKwONfg13z3ImWsDLu6TIMez6IxU1jkcvJTmkp3ug8hyAMjSLj0oqO12DgM8cOOnNHG+XFnEBySm6atZzrQ70zCJ7ujYfMfMPa3i6ysj67p7hwYjz/Umt4NAPnyzS4Byj5c54D21ug/RgPx+rktsjHU= jinxiang@nvidia.com]
    install-server: true
  disk_setup:
    ephemeral0:
      table_type: "mbr"
      layout: true
    /dev/nvme0n1:
      table_type: mbr
      layout: true
    /dev/nvme1n1:
      table_type: mbr
      layout:
        - 33
        - [33, 82]
        - 33
      overwrite: True
  runcmd:
    - [sh, -c, "echo 8 > /sys/class/net/ens5/device/sriov_numvfs"]
    - [sh, -c, "ifconfig ens5v0 up"]
`

var TestCommonInvalidCloudInit = `#cloud-config
package_update: true
this_is_a_bare_scaler_which_is_invalid_yaml_when_in_a_map
packages:
    - net-tools
autoinstall:
    version: 1
    identity:
        hostname: qapreview
        password: $6$jCfWFbdxh1lK09sY$pxFnrW/yXewYFmgoaywu3WKhdPQg0e8DR8jvedAV.udXM0.i5M6wr4Up2S7ZCN9kNDmg.s7fmrOaXE6nEyzPb/ # Welcome123
        username: ubuntu
    ntp:
        enabled: true
        ntp_client: chrony # Uses cloud-init default chrony configuration
        servers:
            - 132.163.96.5
    keyboard:
        layout: us
        toggle: null
        variant: ""
    locale: en_US
    network:
        version: 2
        ethernets:
            ens5f0:
                critical: true
                dhcp-identifier: mac
                dhcp4: true
                nameservers:
                    addresses: [8.8.8.8]
    ssh:
        allow-pw: true
        authorized-keys: [ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQC5bGg4eGyprn25wNEZ2bSiNYvYfLPcvRUNNI2CDx22YNG6+IXt9X+Y4CiTJ14k9QGFnx+vAzNGoLBgvtiwLhQrxOadSypH9KZWTadYK+yR8EWOEAOfgnCf++Vrh2eEKZAayPvWEO6xVgZod+GZtU69JZLv2aO1hMiBPGB4bP4ieFO0oKVM7dZenYg/I7uTppr5NKPkj/xIT7eB+tR3TeSZ44WaK1gZOgh7GL72CCKI5BuuAePXDYZFt0+rPX+kwViDBxzVRDbDwEgFg7NGJFgqt8dPlflw5w6MANvKSJ69yBdxVoJdtANc7mNm4h8RVMT/a1bK0uXADT18hJ2B4YVJRPHposeCM5XtWKXww4yTqKwONfg13z3ImWsDLu6TIMez6IxU1jkcvJTmkp3ug8hyAMjSLj0oqO12DgM8cOOnNHG+XFnEBySm6atZzrQ70zCJ7ujYfMfMPa3i6ysj67p7hwYjz/Umt4NAPnyzS4Byj5c54D21ug/RgPx+rktsjHU= jinxiang@nvidia.com]
        install-server: true
    disk_setup:
        ephemeral0:
            table_type: "mbr"
            layout: true
        /dev/nvme0n1:
            table_type: mbr
            layout: true
        /dev/nvme1n1:
            table_type: mbr
            layout:
                - 33
                - [33, 82]
                - 33
            overwrite: True
    runcmd:
        - [sh, -c, "echo 8 > /sys/class/net/ens5/device/sriov_numvfs"]
        - [sh, -c, "ifconfig ens5v0 up"]
`

var TestCommonPhoneHomeSegment = `
phone_home:
  empty_key_for_validation:
  url: http://169.254.169.254/latest/meta-data/phone_home
  post: all`

var TestCommonPhoneHomeCloudInit = `#cloud-config
package_update: true
phone_home:
  url: http://169.254.169.254/latest/meta-data/phone_home #TestCommonPhoneHomeCloudInit
  post: all`

var TestCommonPhoneHomeOnlyCloudInit = `#cloud-config
phone_home:
  url: http://169.254.169.254/latest/meta-data/phone_home
  post: all`

var TestCommonXMLUserData = `<?xml version="1.0" encoding="UTF-8"?>
<server>
    <autoinstall>
        <version>1</version>
        <identity>
            <hostname>qapreview</hostname>
            <password>$6$jCfWFbdxh1lK09sY$pxFnrW/yXewYFmgoaywu3WKhdPQg0e8DR8jvedAV.udXM0.i5M6wr4Up2S7ZCN9kNDmg.s7fmrOaXE6nEyzPb/</password>
            <username>ubuntu</username>
        </identity>
    </autoinstall>
</server>
</xml>`

var (
	CharsetAlphaNumeric = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
)

// GenerateRandomString generates a random string of a given length from a specified character set.
func GenerateRandomString(length int, charset string) string {
	b := make([]byte, length)
	for i := range b {
		b[i] = charset[rand.Intn(len(charset))]
	}
	return string(b)
}
