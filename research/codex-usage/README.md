# Codex usage (`/wham/usage`)

Source: `openai/codex` at `a07c8eb`. The reply shape is generated from the backend OpenAPI spec: `codex-rs/codex-backend-openapi-models/src/models/rate_limit_status_payload.rs`, `rate_limit_status_details.rs` and `rate_limit_window_snapshot.rs`. The client that sends the request is `codex-rs/backend-client/src/client.rs`.

## Request

`GET https://chatgpt.com/backend-api/wham/usage`, with the headers `Authorization: Bearer <access token>` and `ChatGPT-Account-Id: <account id>`.

## Reply

The body is an object with `plan_type` and five optional keys: `rate_limit`, `credits`, `spend_control`, `additional_rate_limits` and `rate_limit_reached_type`. Any optional key may be absent or null.

`rate_limit` is an object with `allowed`, `limit_reached`, and two optional windows: `primary_window` and `secondary_window`. Each window is an object with four integers: `used_percent`, `limit_window_seconds`, `reset_after_seconds` and `reset_at`.

`used_percent` runs from 0 to 100. Codex documents its `RateLimitWindow` as a percentage from 0 to 100. `reset_at` is Unix seconds: codex passes it straight to `resets_at`, which it documents as a Unix timestamp in seconds since the epoch.

The remaining keys describe credit and overage, which Fiber ignores. `credits` is an object with `has_credits`, `unlimited` and an optional `balance`. `spend_control` is an object with `reached` and an optional `individual_limit`. `additional_rate_limits` is a list of objects with `limit_name`, `metered_feature` and `rate_limit`; codex prefers the snapshot whose `limit_id` is `codex`. `rate_limit_reached_type` is an object with `type`, one of `rate_limit_reached`, `workspace_owner_credits_depleted`, `workspace_member_credits_depleted`, `workspace_owner_usage_limit_reached`, `workspace_member_usage_limit_reached` or `unknown`.

## How Fiber maps it

`quota()` returns `{ windows = { { name, used_percent, resets_at } } }`, one entry per present window, primary first, with `resets_at` set to the window's `reset_at`. A window that is absent or null is skipped. With no `rate_limit`, or with neither window present, `quota()` returns nothing. Every other field is ignored.
