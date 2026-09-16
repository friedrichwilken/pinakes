# Rotate signing keys

Signing keys sign download links. Rotate them every 90 days.

## Rotate

Run `service keys rotate`; the old key stays valid for one hour so links in flight
keep working.
