# marius

**Marius** is a data-oriented reactive projection engine for PostgreSQL-backed applications.

It transforms a stream of database mutations (via `LISTEN/NOTIFY`) into static or semi-static HTML artifacts using ahead-of-time (AOT) compilation, eliminating intermediate caches, ORM layers, and JSON serialization.

## Architecture

```
PostgreSQL (source of truth)
    │  LISTEN/NOTIFY
    ▼
Collector<MAX, WORDS>   lock-free bit-vector, O(1) dedup
    │  flush (tick or threshold)
    ▼
Dispatcher<P>           adaptive tick, parallel Rayon render
    │
    ▼
Artifact (HTML file / RAM)
```

## Crates

| Crate                   | Role                                                                                          |
| ----------------------- | --------------------------------------------------------------------------------------------- |
| `marius-collector`      | `Collector<MAX, WORDS>`, `Dispatcher`, `Projection` trait                                     |
| `marius-schema`         | Generated `#[repr(C)]` structs (DB-Forge)                                                     |
| `marius-projection`     | Generated `impl Projection` (Bridge-Forge + Fragment-Forge)                                   |
| `marius-render`         | Artifact I/O, Axum integration                                                                |
| `marius-db-forge`       | Build-time: `pg_attribute` → Rust structs                                                     |
| `marius-fragment-forge` | Build-time: `.marius` template parsing and native Rust compilation (`push_str` / `write_fmt`) |
| `marius-guard-forge`    | Build-time: RLS / security trait generation                                                   |
| `marius-bridge-forge`   | Build-time: SQLx batch query generation                                                       |

## Status

Early development — API unstable.

## License

MIT OR Apache-2.0
