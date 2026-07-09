# marius-db-forge

Build-time code generator for the [Marius](https://crates.io/crates/marius) engine.

Introspects PostgreSQL `pg_attribute` and emits `#[repr(C)]` structs, `Collector` statics, and `Projection` stubs into `$OUT_DIR/generated_schema.rs`.

Used exclusively as a `[build-dependency]` — never appears in runtime dependency graphs.

## Status

Early development — API unstable.

## License

MIT OR Apache-2.0
