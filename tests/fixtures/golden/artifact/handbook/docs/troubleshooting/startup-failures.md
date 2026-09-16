# Startup failures

The service fails to start when the configuration file is missing, the port is
taken or the storage directory is not writable. The first log line names the cause.

## Fails to start on boot

When the service starts before the network is up, the object store
endpoint is unreachable. Order the unit after `network-online.target`.
