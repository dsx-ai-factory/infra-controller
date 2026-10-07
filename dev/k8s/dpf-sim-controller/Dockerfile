# Build. Base images are digest-pinned so rebuilds are reproducible; bump the
# tag AND digest together (docker buildx imagetools inspect <ref> | head -1).
FROM golang:1.25@sha256:9006890ecba0a168034d99516084099ae3114d9f2b7d6572c77f2dde57ebc980 AS build
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
