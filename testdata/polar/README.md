# Polar's answers, for the apps' license tests

The three apps (macOS, Windows, Linux) ask Polar's public license-key endpoints themselves and report what Polar answered to the agent (`RecordLicenseResult`, see `docs/architecture.md`, "License"). These files are the response bodies their parsers must handle, so the three test suites read the same ones. `scripts/mock-polar.py` serves the same bodies.

| File | Endpoint and status | What the app makes of it |
|---|---|---|
| `activate-200.json` | `activate`, 200 | Activated: keep `id` (the activation id) and `license_key.customer.email` |
| `validate-200.json` | `validate`, 200 | Still licensed: record the time (the `Date` header) and `customer.email` |
| `activate-403-limit.json` | `activate`, 403 `NotPermitted` | Every seat taken, or the key no longer active: the app keys on status codes, not on Polar's wording, so it tells the two apart with a `validate` that sends no `activation_id` (`validate-200-key-only.json` means seats full, a 404 means invalid) |
| `activate-403-revoked.json` | `activate`, 403 `NotPermitted` | The same, for a refunded or revoked key |
| `validate-200-key-only.json` | `validate` without `activation_id`, 200 | The key is live (`status` `granted`, `activation` null): after a 403 from `activate`, seats full |
| `404-not-found.json` | `activate` or `validate`, 404 `ResourceNotFound` | Invalid key, or this computer's activation is gone; `validate` changes the state only after two 404s at least a minute apart |
| `404-no-longer-active.json` | `validate`, 404 `ResourceNotFound` | The same as above: refunded or revoked |
| `422-validation.json` | any, 422 | A request Polar could not read. Invalid only when Polar's `detail` points at the key (for validate and deactivate, the key or the activation id); any other 422 (for example a label Polar rejects) is not a refusal: the app says it couldn't save the license |
| `403-cloudflare.json` | any, 403 without Polar's `error` field | Cloudflare in front of Polar refused the client, not Polar: try again later, never "seats full" |
| `429-rate-limited.json` | any, 429 with `Retry-After` | Try again after `Retry-After` seconds |

`deactivate` answers 204 with no body; a 404 there means the activation is already gone, which counts as done.

The bodies follow what Polar's API returned in its sandbox in October 2026; fields an app does not read may come and go. Apps read only the fields named above and ignore the rest.
