# polyphony-sqlite

`polyphony-sqlite` adds optional persistence through `sqlx`.

## Responsibility

It implements the `StateStore` trait for:

- bootstrap recovery data
- persisted run records
- budget snapshots

## Why it is optional

The main runtime does not require a database. This crate lets deployments add persistence without
forcing SQLite into the rest of the workspace.
