# Back up and restore

A backup is the database dump plus the storage backend contents.

## Back up

Run `service backup create` while the service is running; it writes a consistent
snapshot to the backup directory.

## Restore

Stop the service, run `service backup restore <snapshot>` and start it again.
