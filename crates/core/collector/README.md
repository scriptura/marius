# marius-collector

Lock-free `Collector<MAX, WORDS>` and reactive `Dispatcher` for the Marius engine.

## Components

**`Collector<MAX, WORDS>`** — bit-vector presence table. Receives PostgreSQL `LISTEN/NOTIFY` signals, deduplicates in O(1) via atomic `fetch_or`. Signals beyond `MAX` are counted as `dropped` (configuration drift indicator, not overflow).

**`Dispatcher<P, MAX, WORDS>`** — drains the Collector on volumetric threshold or temporal tick. Fetches records via `P::fetch_batch`, renders via `P::render` (parallel Rayon), writes artifacts. Adaptive tick (100ms–2000ms) based on batch size and render time.

**`Projection` trait** — implemented by generated crates (`marius-projection`). Connects the Dispatcher to table-specific fetch and render logic.

## Status

Early development — API unstable.

## License

MIT OR Apache-2.0
