# Container image

The `Dockerfile` builds a small image (`FROM scratch`) holding one static
binary for `linux/arm64` or `linux/amd64`.

Each release (a `vX.Y.Z` tag on `main`) publishes it as
`ghcr.io/pecheriere/coffer:X.Y.Z` for both platforms, after CI and the
dependency-age check pass again on the tagged commit, with a build-provenance
attestation. Pin the digest and verify the attestation before deploying:

```sh
docker buildx imagetools inspect ghcr.io/pecheriere/coffer:X.Y.Z   # the digest
gh attestation verify oci://ghcr.io/pecheriere/coffer@sha256:<digest> \
  --repo pecheriere/coffer --source-ref refs/tags/vX.Y.Z
```

Or build it yourself:

```sh
docker buildx build --platform linux/arm64 --load -t coffer:dev .
```

The builder cross-compiles, so either platform builds on either kind of host
without emulation.

## What the image provides

| | |
|---|---|
| Binary | `/coffer`; `ENTRYPOINT ["/coffer"]`, `CMD ["serve"]` |
| User | `65532:65532`, no shell |
| CA certificates | `/etc/ssl/certs/ca-certificates.crt`, copied from the builder's Alpine `ca-certificates-bundle`, to verify provider TLS certificates |
| Data | `VOLUME /data`, created with mode 0700 and owned by `65532:65532` |
| Environment | `COFFER_DATA_DIR=/data`, `COFFER_LISTEN=0.0.0.0:8080`, `SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt` |
| Port | `8080` (plain HTTP) |
| Health check | `["CMD", "/coffer", "healthcheck"]` |

`COFFER_PUBLIC_URL` has no default: set it to the origin users open in their
browser. Every other variable in [configuration.md](configuration.md) can be
set too. The server writes only to `/data`, so it runs with a read-only root
filesystem and no capabilities.

## Running it

```sh
docker run -d --name coffer \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  -v coffer-data:/data \
  -e COFFER_PUBLIC_URL=https://coffer.example.com \
  -p 127.0.0.1:8080:8080 \
  coffer:dev
```

The same with Compose:

```yaml
services:
  coffer:
    image: coffer:dev
    read_only: true
    cap_drop: [ALL]
    security_opt: [no-new-privileges:true]
    environment:
      COFFER_PUBLIC_URL: https://coffer.example.com
    volumes:
      - coffer-data:/data
    ports:
      - 127.0.0.1:8080:8080
    restart: unless-stopped
    stop_grace_period: 30s

volumes:
  coffer-data:
```

`docker stop` sends SIGTERM; coffer finishes in-flight requests and exits.

### The data volume

coffer refuses to start when `/data` is accessible by group or others
(`data directory /data must not be accessible by group or others`). The
directory must be owned by `65532:65532` with mode 0700:

- A new, empty named volume gets both from the image the first time it is
  mounted, so nothing needs doing.
- For a bind mount, create the host directory first, for example
  `install -d -m 0700 -o 65532 -g 65532 /srv/coffer`.
- To fix an existing volume, change it from a throwaway container:

  ```sh
  docker run --rm -v coffer-data:/data alpine sh -c 'chown 65532:65532 /data && chmod 700 /data'
  ```

The volume holds the database and the server key. Keep it on persistent
storage and back it up (see below).

### Reverse proxy

coffer speaks plain HTTP and must sit behind a TLS-terminating reverse proxy
that serves exactly `COFFER_PUBLIC_URL`; the requirements are in
[configuration.md](configuration.md#reverse-proxy-and-tls). Inside the
container coffer binds every interface on port 8080, so publish that port
only where the proxy alone can reach it: on `127.0.0.1` as above, or on a
private Docker network shared with the proxy and not published at all.

### Outbound network

To sync provider connections the container needs outbound https (port 443)
to the hosts in `COFFER_SIMPLEFIN_HOSTS`, and nowhere else. The image
carries a CA bundle to verify their certificates, so no host mount is
needed. Enforce the allow-list outside the container too: route egress
through a proxy that allows `CONNECT` to those hosts only and pass it as
`HTTPS_PROXY`, or restrict the container's network with an SNI or firewall
allow-list. Without any connection, coffer still serves everything except
syncing; see [configuration.md](configuration.md#outbound-network).

## Operator commands

Run the other commands in the running container:

```sh
docker exec coffer /coffer bootstrap-link           # first-admin setup link
docker exec coffer /coffer recover-admin <username> # re-enroll link for a locked-out admin
docker exec coffer /coffer backup /data/<new-file>  # consistent copy of the database
```

On first start, with no admin yet, `serve` also logs the setup link
(`docker logs coffer`). The link is single-use and expires after
`COFFER_INVITE_TTL_MINUTES`.

`/data` is the only writable path in the container, so a backup is written
there first; the destination must not exist yet. Copy it out, then remove
it from a throwaway container, since the image has no `rm`:

```sh
docker exec coffer /coffer backup /data/backup-2026-01-31.db
docker cp coffer:/data/backup-2026-01-31.db ./
docker run --rm -v coffer-data:/data alpine rm /data/backup-2026-01-31.db
```

A backup contains credential public keys, token hashes and all financial
data, and not the server key; see [configuration.md](configuration.md#backups).
