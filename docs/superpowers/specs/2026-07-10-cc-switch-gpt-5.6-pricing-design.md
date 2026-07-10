# CC Switch GPT-5.6 Pricing Design

## Goal

Add built-in pricing for `gpt-5.6-sol`, `gpt-5.6-terra`, and `gpt-5.6-luna` to the CC Switch v3.16.5 custom build that already displays Codex reasoning tokens.

## Source Of Truth

Use OpenAI's June 26, 2026 GPT-5.6 launch announcement:

`https://openai.com/index/previewing-gpt-5-6-sol/`

Prices are USD per one million tokens. OpenAI states that cache reads receive a 90% cached-input discount and cache writes cost 1.25 times the uncached input rate.

| Model ID | Display name | Input | Output | Cache read | Cache creation |
| --- | --- | ---: | ---: | ---: | ---: |
| `gpt-5.6-sol` | GPT-5.6 Sol | 5 | 30 | 0.5 | 6.25 |
| `gpt-5.6-terra` | GPT-5.6 Terra | 2.5 | 15 | 0.25 | 3.125 |
| `gpt-5.6-luna` | GPT-5.6 Luna | 1 | 6 | 0.1 | 1.25 |

## Implementation

Add the three rows to the existing `pricing_data` array in `Database::seed_model_pricing` in `src-tauri/src/database/schema.rs`.

Do not change `SCHEMA_VERSION`. The current startup path calls `ensure_model_pricing_seeded`, which uses `INSERT OR IGNORE`. Existing v12 databases therefore receive missing GPT-5.6 rows on startup while any existing same-ID user prices remain unchanged.

No pricing-repair rule is required because there is no prior built-in GPT-5.6 price to replace.

## Model Matching

Use the existing pricing candidate normalization. It already handles provider path prefixes, OpenAI dot prefixes, date suffixes, colon suffixes, and reasoning-effort suffixes. Production normalization code does not need modification.

Add regression cases showing that representative names resolve to the new bare IDs, including provider-prefixed, date-suffixed, and effort-suffixed forms.

## Tests

- Extend `schema_model_pricing_is_seeded_on_init` in `src-tauri/src/database/tests.rs` to assert all three rows and all four price dimensions.
- Add or extend a database test proving repeated seeding does not overwrite an existing user price for one GPT-5.6 model.
- Extend `test_model_pricing_matching` in `src-tauri/src/services/usage_stats.rs` with representative GPT-5.6 aliases.
- Run the focused Rust tests, the reasoning-token frontend test, TypeScript checking, and a full Windows Actions build.

## Delivery

Push the updated branch `codex/reasoning-token-display-v3.16.5`, download the new successful artifact, back up the live database, replace only `E:\INSTALL\CC Switch Reasoning 3.16.5\cc-switch.exe`, and restart it normally.

After restart, verify that the three model-pricing rows exist in the live v12 database and that recent `gpt-5.6-sol` request logs no longer display `未定价`.
