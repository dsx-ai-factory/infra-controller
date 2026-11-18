# Build. Base images are digest-pinned so rebuilds are reproducible; bump the
# tag AND digest together (docker buildx imagetools inspect <ref> | head -1).
FROM golang:1.27.3@sha256:f20d83a83e18dbd2a4714b8ffd4b30b287b6243962dd75608e86abcc63ba4efc AS build
WORKDIR /src
COPY go.mod go.sum* ./
RUN go mod download
COPY cmd/ cmd/
COPY internal/ internal/
RUN CGO_ENABLED=0 go build -o /out/dpf-sim-controller ./cmd

# Run (distroless: this is dev tooling but keep it small)
FROM gcr.io/distroless/static:nonroot@sha256:f7f8f729987ad0fdf6b05eeeae94b26e6a0f613bdf46feea7fc40f7bd72953e6
WORKDIR /
COPY --from=build /out/dpf-sim-controller .
USER 65532:65532
ENTRYPOINT ["/dpf-sim-controller"]
