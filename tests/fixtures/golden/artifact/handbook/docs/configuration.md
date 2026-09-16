# Configuration

The service reads `service.toml` at start. Every key has a default.

## Server

Keys under `[server]` control listening and TLS.

### Ports

`server.port` is the plain HTTP port (default 8080). `server.tls_port` is the HTTPS port
(default 8443) and is only opened when a certificate is configured.

### TLS

Set `server.tls.cert` and `server.tls.key` to PEM files. The service reloads them when
they change on disk.

## Storage

Keys under `[storage]` select the backend.

### Local disk

`storage.path` names a directory; the service must own it.

### Object store

`storage.bucket`, `storage.region` and `storage.endpoint` select an object store
bucket; credentials come from the environment.

## Logging

`log.level` accepts `error`, `warn`, `info` and `debug`; `log.format` is `text` or
`json`.
