```
cargo bench -p marius-render --bench hot_path_certify
Compiling marius-render v0.3.0 (/home/nunn/Development/GitHub/marius/crates/shell/render)
    Finished `bench` profile [optimized] target(s) in 3.13s
     Running benches/hot_path_certify.rs (target/release/deps/hot_path_certify-8193862e6fa331e2)
Timer precision: 20 ns
hot_path_certify                                     fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ certify/zero_alloc_in_render                      1.121 µs      │ 1.222 µs      │ 1.132 µs      │ 1.135 µs      │ 100     │ 200
├─ certify/zero_alloc_in_render_segments_large_body  369.7 ns      │ 19.24 µs      │ 390.7 ns      │ 582.2 ns      │ 100     │ 100
├─ render/sequential/nominal                                       │               │               │               │         │
│  ├─ 100                                            35.03 µs      │ 43.16 µs      │ 35.81 µs      │ 37.13 µs      │ 100     │ 100
│  │                                                 60.32 GB/s    │ 48.96 GB/s    │ 59 GB/s       │ 56.9 GB/s     │         │
│  │                                                 2.854 Mitem/s │ 2.316 Mitem/s │ 2.791 Mitem/s │ 2.692 Mitem/s │         │
│  ├─ 1000                                           400 µs        │ 654.4 µs      │ 434.5 µs      │ 490.1 µs      │ 100     │ 100
│  │                                                 52.83 GB/s    │ 32.29 GB/s    │ 48.63 GB/s    │ 43.11 GB/s    │         │
│  │                                                 2.499 Mitem/s │ 1.528 Mitem/s │ 2.301 Mitem/s │ 2.04 Mitem/s  │         │
│  ╰─ 10000                                          4.176 ms      │ 4.679 ms      │ 4.347 ms      │ 4.369 ms      │ 100     │ 100
│                                                    50.6 GB/s     │ 45.16 GB/s    │ 48.61 GB/s    │ 48.37 GB/s    │         │
│                                                    2.394 Mitem/s │ 2.136 Mitem/s │ 2.3 Mitem/s   │ 2.288 Mitem/s │         │
├─ render/sequential/worst_case                                    │               │               │               │         │
│  ├─ 100                                            128.1 µs      │ 227.9 µs      │ 130.3 µs      │ 133.1 µs      │ 100     │ 100
│  │                                                 16.48 GB/s    │ 9.27 GB/s     │ 16.21 GB/s    │ 15.87 GB/s    │         │
│  │                                                 780 Kitem/s   │ 438.6 Kitem/s │ 767.2 Kitem/s │ 750.9 Kitem/s │         │
│  ├─ 1000                                           1.287 ms      │ 2.095 ms      │ 1.33 ms       │ 1.346 ms      │ 100     │ 100
│  │                                                 16.4 GB/s     │ 10.08 GB/s    │ 15.88 GB/s    │ 15.69 GB/s    │         │
│  │                                                 776.4 Kitem/s │ 477.1 Kitem/s │ 751.6 Kitem/s │ 742.6 Kitem/s │         │
│  ╰─ 10000                                          12.81 ms      │ 22.19 ms      │ 14.27 ms      │ 14.52 ms      │ 100     │ 100
│                                                    16.49 GB/s    │ 9.52 GB/s     │ 14.8 GB/s     │ 14.54 GB/s    │         │
│                                                    780.4 Kitem/s │ 450.4 Kitem/s │ 700.6 Kitem/s │ 688.3 Kitem/s │         │
├─ render/single/nominal                             415.2 ns      │ 3.982 µs      │ 430.7 ns      │ 487.6 ns      │ 100     │ 200
│                                                    50.89 GB/s    │ 5.306 GB/s    │ 49.06 GB/s    │ 43.34 GB/s    │         │
│                                                    2.408 Mitem/s │ 251 Kitem/s   │ 2.321 Mitem/s │ 2.05 Mitem/s  │         │
╰─ render/single/worst_case                          1.301 µs      │ 3.616 µs      │ 1.322 µs      │ 1.361 µs      │ 100     │ 100
                                                     16.23 GB/s    │ 5.843 GB/s    │ 15.97 GB/s    │ 15.52 GB/s    │         │
                                                     768.1 Kitem/s │ 276.4 Kitem/s │ 755.9 Kitem/s │ 734.6 Kitem/s │         │
```

```
cargo bench -p marius-render --bench hot_path_render
Compiling marius-render v0.3.0 (/home/nunn/Development/GitHub/marius/crates/shell/render)
    Finished `bench` profile [optimized] target(s) in 3.20s
     Running benches/hot_path_render.rs (target/release/deps/hot_path_render-fdbba51ddae789b9)
Timer precision: 20 ns
hot_path_render                       fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ render/segmented/sequential_large                │               │               │               │         │
│  ├─ 10                              111.8 µs      │ 165.6 µs      │ 116.4 µs      │ 118.2 µs      │ 100     │ 100
│  │                                  1.888 GB/s    │ 1.276 GB/s    │ 1.814 GB/s    │ 1.787 GB/s    │         │
│  │                                  89.37 Kitem/s │ 60.38 Kitem/s │ 85.87 Kitem/s │ 84.55 Kitem/s │         │
│  ├─ 100                             1.152 ms      │ 2.521 ms      │ 1.306 ms      │ 1.444 ms      │ 100     │ 100
│  │                                  1.834 GB/s    │ 838.2 MB/s    │ 1.617 GB/s    │ 1.463 GB/s    │         │
│  │                                  86.77 Kitem/s │ 39.66 Kitem/s │ 76.53 Kitem/s │ 69.24 Kitem/s │         │
│  ╰─ 1000                            16.63 ms      │ 22.2 ms       │ 17.1 ms       │ 17.62 ms      │ 100     │ 100
│                                     1.27 GB/s     │ 951.7 MB/s    │ 1.235 GB/s    │ 1.199 GB/s    │         │
│                                     60.1 Kitem/s  │ 45.03 Kitem/s │ 58.47 Kitem/s │ 56.73 Kitem/s │         │
├─ render/segmented/single_large      380.4 ns      │ 2.512 µs      │ 390.5 ns      │ 416 ns        │ 100     │ 400
│                                     55.55 GB/s    │ 8.412 GB/s    │ 54.11 GB/s    │ 50.8 GB/s     │         │
│                                     2.628 Mitem/s │ 398 Kitem/s   │ 2.56 Mitem/s  │ 2.403 Mitem/s │         │
├─ render/sequential/nominal                        │               │               │               │         │
│  ├─ 100                             37.35 µs      │ 57.88 µs      │ 40.08 µs      │ 40.77 µs      │ 100     │ 100
│  │                                  56.58 GB/s    │ 36.51 GB/s    │ 52.72 GB/s    │ 51.82 GB/s    │         │
│  │                                  2.677 Mitem/s │ 1.727 Mitem/s │ 2.494 Mitem/s │ 2.452 Mitem/s │         │
│  ├─ 1000                            419.7 µs      │ 590.6 µs      │ 443.9 µs      │ 447.5 µs      │ 100     │ 100
│  │                                  50.35 GB/s    │ 35.78 GB/s    │ 47.6 GB/s     │ 47.21 GB/s    │         │
│  │                                  2.382 Mitem/s │ 1.693 Mitem/s │ 2.252 Mitem/s │ 2.234 Mitem/s │         │
│  ╰─ 10000                           4.21 ms       │ 4.447 ms      │ 4.293 ms      │ 4.296 ms      │ 100     │ 100
│                                     50.19 GB/s    │ 47.52 GB/s    │ 49.23 GB/s    │ 49.18 GB/s    │         │
│                                     2.374 Mitem/s │ 2.248 Mitem/s │ 2.329 Mitem/s │ 2.327 Mitem/s │         │
├─ render/sequential/worst_case                     │               │               │               │         │
│  ├─ 100                             132.6 µs      │ 160.1 µs      │ 134 µs        │ 135 µs        │ 100     │ 100
│  │                                  15.93 GB/s    │ 13.19 GB/s    │ 15.76 GB/s    │ 15.65 GB/s    │         │
│  │                                  753.8 Kitem/s │ 624.5 Kitem/s │ 745.9 Kitem/s │ 740.5 Kitem/s │         │
│  ├─ 1000                            1.223 ms      │ 1.483 ms      │ 1.317 ms      │ 1.318 ms      │ 100     │ 100
│  │                                  17.28 GB/s    │ 14.24 GB/s    │ 16.04 GB/s    │ 16.02 GB/s    │         │
│  │                                  817.6 Kitem/s │ 673.8 Kitem/s │ 759 Kitem/s   │ 758.4 Kitem/s │         │
│  ╰─ 10000                           13.05 ms      │ 14.29 ms      │ 13.35 ms      │ 13.41 ms      │ 100     │ 100
│                                     16.18 GB/s    │ 14.78 GB/s    │ 15.82 GB/s    │ 15.75 GB/s    │         │
│                                     765.8 Kitem/s │ 699.6 Kitem/s │ 748.7 Kitem/s │ 745.6 Kitem/s │         │
├─ render/single/nominal              432.9 ns      │ 2.236 µs      │ 450.4 ns      │ 469.2 ns      │ 100     │ 400
│                                     48.81 GB/s    │ 9.45 GB/s     │ 46.91 GB/s    │ 45.04 GB/s    │         │
│                                     2.309 Mitem/s │ 447.1 Kitem/s │ 2.219 Mitem/s │ 2.131 Mitem/s │         │
╰─ render/single/worst_case           1.357 µs      │ 1.407 µs      │ 1.372 µs      │ 1.371 µs      │ 100     │ 200
                                      15.57 GB/s    │ 15.01 GB/s    │ 15.4 GB/s     │ 15.4 GB/s     │         │
                                      736.8 Kitem/s │ 710.6 Kitem/s │ 728.7 Kitem/s │ 728.9 Kitem/s │         │
```