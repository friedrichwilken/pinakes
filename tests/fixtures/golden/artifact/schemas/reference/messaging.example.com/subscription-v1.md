# Subscription (messaging.example.com/v1)

A Subscription describes interest in a class of events.

Scope: Namespaced · Plural: subscriptions · Short names: sub, subs · Served: yes · Storage: yes

## Fields

| Field | Type | Required | Values | Description |
|---|---|---|---|---|
| `spec.config.*` | string | no |  |  |
| `spec.extra` | object | no | preserves unknown fields |  |
| `spec.filters[].eventType` | string | no |  |  |
| `spec.sink` | string | yes |  | The URL of the subscriber. |
| `spec.typeMatching` | string | no | `exact`, `standard` | How the event type is matched. |

## Status

| Field | Type | Values | Description |
|---|---|---|---|
| `ready` | boolean |  |  |

## Conditions

| Type | Description |
|---|---|
| `Ready` | The kind of condition. |
| `Subscribed` | The kind of condition. |
