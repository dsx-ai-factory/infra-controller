# Option A — download and build in one command, 3.2.2 

```bash
./build-dpu-install-iso.sh \
  --control-plane-config site-sample.yaml   \
  --download-artifacts   --doca-version 3.2.2 \
  --bfb-build 125  \
  --bfb-release 26.02  \
  --hbn-version 3.2.2  \
  --hbn-container-tag 3.2.2-doca3.2.2  \
  --doca-host-url https://www.mellanox.com/downloads/DOCA/DOCA_v2.10.0/host/doca-host_2.10.0-093000-25.01-ubuntu2404_amd64.deb   \
  --rshim-url     https://github.com/Mellanox/rshim-user-space/releases/download/rshim-2.3.1/rshim_2.3.1_amd64.deb \
  --libfuse2-url http://archive.ubuntu.com/ubuntu/pool/universe/f/fuse/libfuse2t64_2.9.9-8.1build1_amd64.deb \
  --output-dir ~/doca_3.2.2_hbn_3.2.2
```

# Option B — download artifacts first, then build

```bash
mkdir -p ./artifacts && cd ./artifacts
../download-build-dpu-artifacts.sh \
  --doca-version      3.2.2 \
  --bfb-build         125 \
  --bfb-release       26.02 \
  --hbn-container-tag 3.2.2-doca3.2.2 \
  --doca-host-url https://www.mellanox.com/downloads/DOCA/DOCA_v2.10.0/host/doca-host_2.10.0-093000-25.01-ubuntu2404_amd64.deb \
  --rshim-url     https://github.com/Mellanox/rshim-user-space/releases/download/rshim-2.3.1/rshim_2.3.1_amd64.deb \
  --libfuse2-url  http://archive.ubuntu.com/ubuntu/pool/universe/f/fuse/libfuse2t64_2.9.9-8.1build1_amd64.deb
cd ..

./build-dpu-install-iso.sh \
  --control-plane-config site-sample.yaml \
  --doca-version  3.2.2 \
  --hbn-version   3.2.2 \
  --artifacts-dir ./artifacts \
  --output-dir    ./output
```


# Download DOCA 2.9.2 / HBN 2.4.2
```bash
./build-dpu-install-iso.sh  \
  --control-plane-config site-sample.yaml \
  --download-artifacts  \
  --doca-version 2.9.2  \
  --bfb-build 32  \
  --bfb-release 25.02 \
  --hbn-version 2.4.2  \
  --hbn-container-tag 2.4.2-doca2.9.2-32  \
  --doca-host-url https://www.mellanox.com/downloads/DOCA/DOCA_v2.10.0/host/doca-host_2.10.0-093000-25.01-ubuntu2404_amd64.deb   \
	--rshim-url     https://github.com/Mellanox/rshim-user-space/releases/download/rshim-2.3.1/rshim_2.3.1_amd64.deb \
	--libfuse2-url http://archive.ubuntu.com/ubuntu/pool/universe/f/fuse/libfuse2t64_2.9.9-8.1build1_amd64.deb \
	--output-dir ~/doca_2.9.2_hbn_2.4.2

```