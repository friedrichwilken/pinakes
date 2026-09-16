# Architecture

Three components make up a deployment: the API server, the worker pool and the
storage backend. Clients talk to the API server only.

## How the components are connected

The API server enqueues long-running work; workers pick it up
from the queue and write results to the storage backend. Both read the same configuration file.
