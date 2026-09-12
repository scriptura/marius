# Benchs du 12 septembre 2026

Remplacement du transport Axum par Hyper (post ADR-011)

---

## Setup

```
$ sudo lshw -short
Chemin matériel    Périphérique  Classe         Description
==============================================================
                                    system         EQ (Default string)
/0                                  bus            EQ
/0/0                                memory         64KiB BIOS
/0/b                                memory         16GiB Mémoire Système
/0/b/0                              memory         8GiB SODIMM DDR4 Synchrone Unbuffered (Unregistered) 3200 MHz (0.3 ns)
/0/b/1                              memory         8GiB SODIMM DDR4 Synchrone Unbuffered (Unregistered) 3200 MHz (0.3 ns)
/0/d                                memory         512KiB L1 cache
/0/e                                memory         4MiB L2 cache
/0/f                                memory         16MiB L3 cache
/0/10                               processor      AMD Ryzen 7 5825U with Radeon Graphics
```

```
$ lscpu | grep -E "MHz"
multiplication des MHz du/des CPU(s) :    38%
Vitesse maximale du processeur en MHz :   4547.9458
Vitesse minimale du processeur en MHz :   410.9590
```

## Métriques

```
$ cargo bench -p marius-render --bench hot_path_certify
Timer precision: 20 ns
hot_path_certify                                   fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ certify/zero_alloc_in_render                    1.166 µs      │ 1.292 µs      │ 1.182 µs      │ 1.184 µs      │ 100     │ 200
├─ certify/zero_alloc_in_render_chunks_large_body  389.7 ns      │ 18.5 µs       │ 409.7 ns      │ 590.6 ns      │ 100     │ 100
├─ render/sequential/nominal                                     │               │               │               │         │
│  ├─ 100                                          38.51 µs      │ 47.78 µs      │ 39.23 µs      │ 39.81 µs      │ 100     │ 100
│  │                                               55.65 GB/s    │ 44.86 GB/s    │ 54.63 GB/s    │ 53.83 GB/s    │         │
│  │                                               2.596 Mitem/s │ 2.092 Mitem/s │ 2.548 Mitem/s │ 2.511 Mitem/s │         │
│  ├─ 1000                                         422.2 µs      │ 518.6 µs      │ 484.9 µs      │ 481.3 µs      │ 100     │ 100
│  │                                               50.77 GB/s    │ 41.33 GB/s    │ 44.2 GB/s     │ 44.53 GB/s    │         │
│  │                                               2.368 Mitem/s │ 1.928 Mitem/s │ 2.062 Mitem/s │ 2.077 Mitem/s │         │
│  ╰─ 10000                                        4.562 ms      │ 4.881 ms      │ 4.673 ms      │ 4.686 ms      │ 100     │ 100
│                                                  46.98 GB/s    │ 43.91 GB/s    │ 45.87 GB/s    │ 45.74 GB/s    │         │
│                                                  2.191 Mitem/s │ 2.048 Mitem/s │ 2.139 Mitem/s │ 2.133 Mitem/s │         │
├─ render/sequential/worst_case                                  │               │               │               │         │
│  ├─ 100                                          135.1 µs      │ 228.2 µs      │ 141.1 µs      │ 144.7 µs      │ 100     │ 100
│  │                                               15.85 GB/s    │ 9.391 GB/s    │ 15.18 GB/s    │ 14.81 GB/s    │         │
│  │                                               739.6 Kitem/s │ 438.1 Kitem/s │ 708.2 Kitem/s │ 691 Kitem/s   │         │
│  ├─ 1000                                         1.385 ms      │ 1.473 ms      │ 1.412 ms      │ 1.414 ms      │ 100     │ 100
│  │                                               15.47 GB/s    │ 14.54 GB/s    │ 15.17 GB/s    │ 15.15 GB/s    │         │
│  │                                               721.9 Kitem/s │ 678.6 Kitem/s │ 707.9 Kitem/s │ 706.8 Kitem/s │         │
│  ╰─ 10000                                        13.53 ms      │ 16.01 ms      │ 14.14 ms      │ 14.09 ms      │ 100     │ 100
│                                                  15.84 GB/s    │ 13.38 GB/s    │ 15.15 GB/s    │ 15.21 GB/s    │         │
│                                                  739 Kitem/s   │ 624.4 Kitem/s │ 706.8 Kitem/s │ 709.6 Kitem/s │         │
├─ render/single/nominal                           470.7 ns      │ 1.329 µs      │ 479.3 ns      │ 488.3 ns      │ 100     │ 800
│                                                  45.53 GB/s    │ 16.11 GB/s    │ 44.71 GB/s    │ 43.89 GB/s    │         │
│                                                  2.124 Mitem/s │ 751.9 Kitem/s │ 2.085 Mitem/s │ 2.047 Mitem/s │         │
╰─ render/single/worst_case                        1.362 µs      │ 2.213 µs      │ 1.372 µs      │ 1.386 µs      │ 100     │ 100
                                                   15.73 GB/s    │ 9.683 GB/s    │ 15.61 GB/s    │ 15.46 GB/s    │         │
                                                   733.8 Kitem/s │ 451.7 Kitem/s │ 728.4 Kitem/s │ 721.3 Kitem/s │         │
```

```
$ cargo bench -p marius-render --bench hot_path_render
Timer precision: 20 ns
hot_path_render                       fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ render/segmented/sequential_large                │               │               │               │         │
│  ├─ 10                              110.8 µs      │ 162.5 µs      │ 114.3 µs      │ 116.2 µs      │ 100     │ 100
│  │                                  1.933 GB/s    │ 1.318 GB/s    │ 1.874 GB/s    │ 1.843 GB/s    │         │
│  │                                  90.19 Kitem/s │ 61.51 Kitem/s │ 87.42 Kitem/s │ 86 Kitem/s    │         │
│  ├─ 100                             1.094 ms      │ 1.39 ms       │ 1.158 ms      │ 1.166 ms      │ 100     │ 100
│  │                                  1.958 GB/s    │ 1.541 GB/s    │ 1.85 GB/s     │ 1.837 GB/s    │         │
│  │                                  91.34 Kitem/s │ 71.91 Kitem/s │ 86.34 Kitem/s │ 85.69 Kitem/s │         │
│  ╰─ 1000                            17.67 ms      │ 22.39 ms      │ 18.51 ms      │ 18.59 ms      │ 100     │ 100
│                                     1.212 GB/s    │ 957.3 MB/s    │ 1.157 GB/s    │ 1.152 GB/s    │         │
│                                     56.56 Kitem/s │ 44.65 Kitem/s │ 54 Kitem/s    │ 53.77 Kitem/s │         │
├─ render/segmented/single_large      375.5 ns      │ 568.2 ns      │ 385.5 ns      │ 386.9 ns      │ 100     │ 400
│                                     57.08 GB/s    │ 37.72 GB/s    │ 55.6 GB/s     │ 55.4 GB/s     │         │
│                                     2.662 Mitem/s │ 1.759 Mitem/s │ 2.593 Mitem/s │ 2.584 Mitem/s │         │
├─ render/sequential/nominal                        │               │               │               │         │
│  ├─ 100                             40.69 µs      │ 52.47 µs      │ 42.19 µs      │ 42.49 µs      │ 100     │ 100
│  │                                  52.67 GB/s    │ 40.85 GB/s    │ 50.8 GB/s     │ 50.44 GB/s    │         │
│  │                                  2.457 Mitem/s │ 1.905 Mitem/s │ 2.369 Mitem/s │ 2.353 Mitem/s │         │
│  ├─ 1000                            396.1 µs      │ 485.1 µs      │ 424.5 µs      │ 427.7 µs      │ 100     │ 100
│  │                                  54.11 GB/s    │ 44.18 GB/s    │ 50.48 GB/s    │ 50.11 GB/s    │         │
│  │                                  2.524 Mitem/s │ 2.061 Mitem/s │ 2.355 Mitem/s │ 2.337 Mitem/s │         │
│  ╰─ 10000                           4.107 ms      │ 4.413 ms      │ 4.224 ms      │ 4.232 ms      │ 100     │ 100
│                                     52.18 GB/s    │ 48.57 GB/s    │ 50.73 GB/s    │ 50.65 GB/s    │         │
│                                     2.434 Mitem/s │ 2.265 Mitem/s │ 2.366 Mitem/s │ 2.362 Mitem/s │         │
├─ render/sequential/worst_case                     │               │               │               │         │
│  ├─ 100                             136.7 µs      │ 150.6 µs      │ 139.1 µs      │ 139.7 µs      │ 100     │ 100
│  │                                  15.68 GB/s    │ 14.23 GB/s    │ 15.41 GB/s    │ 15.34 GB/s    │         │
│  │                                  731.4 Kitem/s │ 663.9 Kitem/s │ 718.8 Kitem/s │ 715.7 Kitem/s │         │
│  ├─ 1000                            1.364 ms      │ 1.429 ms      │ 1.398 ms      │ 1.398 ms      │ 100     │ 100
│  │                                  15.7 GB/s     │ 14.99 GB/s    │ 15.33 GB/s    │ 15.32 GB/s    │         │
│  │                                  732.7 Kitem/s │ 699.6 Kitem/s │ 715.1 Kitem/s │ 714.8 Kitem/s │         │
│  ╰─ 10000                           14.17 ms      │ 15.51 ms      │ 14.81 ms      │ 14.71 ms      │ 100     │ 100
│                                     15.12 GB/s    │ 13.81 GB/s    │ 14.47 GB/s    │ 14.56 GB/s    │         │
│                                     705.6 Kitem/s │ 644.4 Kitem/s │ 675.2 Kitem/s │ 679.5 Kitem/s │         │
├─ render/single/nominal              421.7 ns      │ 458.1 ns      │ 433.1 ns      │ 432.9 ns      │ 100     │ 800
│                                     50.82 GB/s    │ 46.79 GB/s    │ 49.49 GB/s    │ 49.51 GB/s    │         │
│                                     2.37 Mitem/s  │ 2.182 Mitem/s │ 2.308 Mitem/s │ 2.309 Mitem/s │         │
╰─ render/single/worst_case           1.461 µs      │ 2.053 µs      │ 1.472 µs      │ 1.48 µs       │ 100     │ 100
                                      14.66 GB/s    │ 10.43 GB/s    │ 14.55 GB/s    │ 14.47 GB/s    │         │
                                      684.1 Kitem/s │ 486.9 Kitem/s │ 678.9 Kitem/s │ 675.4 Kitem/s │         │
```

---

# Analyse synthétique des benchmarks

### Ce qui s'améliore
- **Chemins nominaux** : gains systématiques de 1 à 13 % sur presque toutes les tailles, avec des débits mémoire en hausse (jusqu'à +8 % sur `sequential/nominal/100`).
- **Petits et moyens volumes** : `single`, `single_large`, `worst_case/100` et `/1000` bénéficient clairement du passage à Hyper.
- **`certify`** : neutre à légèrement positif.

### Ce qui se dégrade
- **`render/segmented/sequential_large/1000`** : régression de +25 % en temps médian, avec effondrement du débit (−22 %). C'est le point d'attention principal.
- **`worst_case/10000`** (certify et render) : stagnation ou très légère dégradation (+0.6 à +2.8 %), probablement dans le bruit mais à surveiller.
- **Variance** : les `slowest` restent élevés et variables (ex. `segmented/sequential_large/1000` : 15.28 ms → 22.39 ms), ce qui peut indiquer des contentions ou des allocations ponctuelles sous Hyper.

### Interprétation
Le remplacement Axum → Hyper apporte un **gain global sur les chemins nominaux et les petites/moyennes charges**, cohérent avec un transport plus léger et un meilleur contrôle des buffers. En revanche, le **gros volume segmenté** (`segmented/sequential_large/1000`) souffre d'une régression sévère, probablement due à la gestion des corps de grande taille ou à un changement de stratégie de buffering/backpressure dans Hyper.

### Recommandations

1. **Investiguer en priorité** `render/segmented/sequential_large/1000` : profiler l'allocation, la copie de buffers et la gestion des chunks sous Hyper. La régression de +25 % est trop importante pour être ignorée.
2. **Surveiller** `worst_case/10000` sur les deux benches : la légère dégradation pourrait s'accentuer avec des charges plus lourdes.
3. **Consolider les gains** : les améliorations sur `nominal` et `single` sont réelles et exploitables ; elles valident le choix d'Hyper pour les charges courantes.
4. **Reproduire** les mesures avec plusieurs runs pour confirmer que les variations < 3 % ne sont pas du bruit (la variance des `slowest` est élevée).
5. **Ne pas comparer** ces résultats aux benchs antérieurs au 04/09 (avertissement explicite des auteurs).

**Verdict** : le passage à Hyper est globalement bénéfique sur les chemins nominaux et les charges légères/moyennes, mais introduit une régression critique sur le rendu segmenté à grande échelle qu'il convient de corriger avant généralisation.
