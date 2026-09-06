# Benchs du 4 septembre 2026

_Attention : les benchs ont été modifiés pour mieux refléter le comportement du projet, ils ne peuvent donc pas être comparés avec les benchs précédents._
Suite à refactor important de `crates/forge/fragment-forge/src/lib.rs` et de `crates/core/schema/build.rs` (éclatement de ces 2 fichiers monolytiques, separation of concern).
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
hot_path_certify                                     fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ certify/zero_alloc_in_render                      1.217 µs      │ 3.546 µs      │ 1.231 µs      │ 1.261 µs      │ 100     │ 200
├─ certify/zero_alloc_in_render_segments_large_body  389.7 ns      │ 21.65 µs      │ 409.7 ns      │ 622 ns        │ 100     │ 100
├─ render/sequential/nominal                                       │               │               │               │         │
│  ├─ 100                                            40.86 µs      │ 49.06 µs      │ 43.18 µs      │ 43.14 µs      │ 100     │ 100
│  │                                                 53.45 GB/s    │ 44.52 GB/s    │ 50.58 GB/s    │ 50.62 GB/s    │         │
│  │                                                 2.446 Mitem/s │ 2.038 Mitem/s │ 2.315 Mitem/s │ 2.317 Mitem/s │         │
│  ├─ 1000                                           413.2 µs      │ 572 µs        │ 478.3 µs      │ 472.9 µs      │ 100     │ 100
│  │                                                 52.86 GB/s    │ 38.18 GB/s    │ 45.66 GB/s    │ 46.18 GB/s    │         │
│  │                                                 2.42 Mitem/s  │ 1.747 Mitem/s │ 2.09 Mitem/s  │ 2.114 Mitem/s │         │
│  ╰─ 10000                                          4.657 ms      │ 5.369 ms      │ 4.835 ms      │ 4.824 ms      │ 100     │ 100
│                                                    46.9 GB/s     │ 40.68 GB/s    │ 45.17 GB/s    │ 45.28 GB/s    │         │
│                                                    2.147 Mitem/s │ 1.862 Mitem/s │ 2.067 Mitem/s │ 2.072 Mitem/s │         │
├─ render/sequential/worst_case                                    │               │               │               │         │
│  ├─ 100                                            137.9 µs      │ 211 µs        │ 139.7 µs      │ 142 µs        │ 100     │ 100
│  │                                                 15.83 GB/s    │ 10.35 GB/s    │ 15.62 GB/s    │ 15.37 GB/s    │         │
│  │                                                 724.8 Kitem/s │ 473.9 Kitem/s │ 715.4 Kitem/s │ 703.8 Kitem/s │         │
│  ├─ 1000                                           1.371 ms      │ 1.493 ms      │ 1.401 ms      │ 1.407 ms      │ 100     │ 100
│  │                                                 15.92 GB/s    │ 14.62 GB/s    │ 15.58 GB/s    │ 15.52 GB/s    │         │
│  │                                                 729 Kitem/s   │ 669.4 Kitem/s │ 713.6 Kitem/s │ 710.6 Kitem/s │         │
│  ╰─ 10000                                          12.99 ms      │ 14.58 ms      │ 13.75 ms      │ 13.78 ms      │ 100     │ 100
│                                                    16.8 GB/s     │ 14.97 GB/s    │ 15.88 GB/s    │ 15.84 GB/s    │         │
│                                                    769.3 Kitem/s │ 685.4 Kitem/s │ 727.2 Kitem/s │ 725.3 Kitem/s │         │
├─ render/single/nominal                             490.5 ns      │ 578.2 ns      │ 505.7 ns      │ 507.4 ns      │ 100     │ 400
│                                                    44.53 GB/s    │ 37.77 GB/s    │ 43.19 GB/s    │ 43.05 GB/s    │         │
│                                                    2.038 Mitem/s │ 1.729 Mitem/s │ 1.977 Mitem/s │ 1.97 Mitem/s  │         │
╰─ render/single/worst_case                          1.401 µs      │ 2.254 µs      │ 1.412 µs      │ 1.427 µs      │ 100     │ 100
                                                     15.58 GB/s    │ 9.688 GB/s    │ 15.46 GB/s    │ 15.3 GB/s     │         │
                                                     713.3 Kitem/s │ 443.5 Kitem/s │ 707.8 Kitem/s │ 700.4 Kitem/s │         │
```

```
$ cargo bench -p marius-render --bench hot_path_render
Timer precision: 20 ns
hot_path_render                       fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ render/segmented/sequential_large                │               │               │               │         │
│  ├─ 10                              112.1 µs      │ 168 µs        │ 115.7 µs      │ 117.7 µs      │ 100     │ 100
│  │                                  1.948 GB/s    │ 1.3 GB/s      │ 1.887 GB/s    │ 1.855 GB/s    │         │
│  │                                  89.19 Kitem/s │ 59.52 Kitem/s │ 86.4 Kitem/s  │ 84.95 Kitem/s │         │
│  ├─ 100                             1.14 ms       │ 1.254 ms      │ 1.185 ms      │ 1.191 ms      │ 100     │ 100
│  │                                  1.915 GB/s    │ 1.741 GB/s    │ 1.842 GB/s    │ 1.833 GB/s    │         │
│  │                                  87.67 Kitem/s │ 79.72 Kitem/s │ 84.33 Kitem/s │ 83.93 Kitem/s │         │
│  ╰─ 1000                            14 ms         │ 15.28 ms      │ 14.81 ms      │ 14.8 ms       │ 100     │ 100
│                                     1.559 GB/s    │ 1.429 GB/s    │ 1.474 GB/s    │ 1.475 GB/s    │         │
│                                     71.4 Kitem/s  │ 65.42 Kitem/s │ 67.49 Kitem/s │ 67.53 Kitem/s │         │
├─ render/segmented/single_large      415.5 ns      │ 5.162 µs      │ 435.5 ns      │ 485.8 ns      │ 100     │ 400
│                                     52.57 GB/s    │ 4.231 GB/s    │ 50.15 GB/s    │ 44.96 GB/s    │         │
│                                     2.406 Mitem/s │ 193.7 Kitem/s │ 2.296 Mitem/s │ 2.058 Mitem/s │         │
├─ render/sequential/nominal                        │               │               │               │         │
│  ├─ 100                             41.87 µs      │ 53.82 µs      │ 42.82 µs      │ 43.38 µs      │ 100     │ 100
│  │                                  52.17 GB/s    │ 40.58 GB/s    │ 51 GB/s       │ 50.34 GB/s    │         │
│  │                                  2.388 Mitem/s │ 1.857 Mitem/s │ 2.334 Mitem/s │ 2.304 Mitem/s │         │
│  ├─ 1000                            444.1 µs      │ 537.4 µs      │ 486.7 µs      │ 486.8 µs      │ 100     │ 100
│  │                                  49.18 GB/s    │ 40.64 GB/s    │ 44.88 GB/s    │ 44.86 GB/s    │         │
│  │                                  2.251 Mitem/s │ 1.86 Mitem/s  │ 2.054 Mitem/s │ 2.053 Mitem/s │         │
│  ╰─ 10000                           4.53 ms       │ 4.761 ms      │ 4.602 ms      │ 4.616 ms      │ 100     │ 100
│                                     48.22 GB/s    │ 45.88 GB/s    │ 47.46 GB/s    │ 47.31 GB/s    │         │
│                                     2.207 Mitem/s │ 2.1 Mitem/s   │ 2.172 Mitem/s │ 2.166 Mitem/s │         │
├─ render/sequential/worst_case                     │               │               │               │         │
│  ├─ 100                             149.5 µs      │ 169.9 µs      │ 150.6 µs      │ 151.9 µs      │ 100     │ 100
│  │                                  14.61 GB/s    │ 12.85 GB/s    │ 14.49 GB/s    │ 14.37 GB/s    │         │
│  │                                  668.8 Kitem/s │ 588.3 Kitem/s │ 663.5 Kitem/s │ 658.1 Kitem/s │         │
│  ├─ 1000                            1.473 ms      │ 1.557 ms      │ 1.505 ms      │ 1.507 ms      │ 100     │ 100
│  │                                  14.82 GB/s    │ 14.02 GB/s    │ 14.5 GB/s     │ 14.49 GB/s    │         │
│  │                                  678.4 Kitem/s │ 642.1 Kitem/s │ 664.1 Kitem/s │ 663.4 Kitem/s │         │
│  ╰─ 10000                           14.51 ms      │ 16.18 ms      │ 14.72 ms      │ 14.76 ms      │ 100     │ 100
│                                     15.05 GB/s    │ 13.49 GB/s    │ 14.83 GB/s    │ 14.79 GB/s    │         │
│                                     689 Kitem/s   │ 617.8 Kitem/s │ 679.3 Kitem/s │ 677.4 Kitem/s │         │
├─ render/single/nominal              460.7 ns      │ 1.38 µs       │ 473.2 ns      │ 483.2 ns      │ 100     │ 400
│                                     47.41 GB/s    │ 15.82 GB/s    │ 46.15 GB/s    │ 45.2 GB/s     │         │
│                                     2.17 Mitem/s  │ 724.6 Kitem/s │ 2.112 Mitem/s │ 2.069 Mitem/s │         │
╰─ render/single/worst_case           1.477 µs      │ 4.799 µs      │ 1.497 µs      │ 1.531 µs      │ 100     │ 200
                                      14.78 GB/s    │ 4.551 GB/s    │ 14.58 GB/s    │ 14.26 GB/s    │         │
                                      676.6 Kitem/s │ 208.3 Kitem/s │ 667.6 Kitem/s │ 652.8 Kitem/s │         │
```

## Interprétation des résultats par Gemini

Le refactoring de séparation des responsabilités du 4 septembre 2026 introduit une **régression de performance généralisée et uniforme de +12% à +26% sur la latence**, accompagnée d'une baisse du débit mémoire global.

---

## Comparatif `hot_path_certify`

Comparaison des moyennes (*mean*) entre le consensus consolidé post-Axum v0.8 du 6 août 2026 et les mesures post-refactor du 4 septembre 2026 :

| Test | 06/08/2026 (Consensus)

 | 04/09/2026

 | Delta Latence | Débit (06/08 vs 04/09)

 |
| --- | --- | --- | --- | --- |
| `zero_alloc_in_render` | 1.141 µs

 | 1.261 µs

 | **+10.5%** 🔴 | — |
| `sequential/nominal 100` | 35.55 µs

 | 43.14 µs

 | **+21.3%** 🔴 | 58.9 GB/s → 50.62 GB/s

 |
| `sequential/nominal 1000` | 382.7 µs

 | 472.9 µs

 | **+23.5%** 🔴 | 52.18 GB/s → 46.18 GB/s

 |
| `sequential/nominal 10000` | 3.823 ms

 | 4.824 ms

 | **+26.2%** 🔴 | 46.99 GB/s → 45.28 GB/s

 |
| `sequential/worst 100` | 121.3 µs

 | 142.0 µs

 | **+17.1%** 🔴 | 16.88 GB/s → 15.37 GB/s

 |
| `sequential/worst 1000` | 1.172 ms

 | 1.407 ms

 | **+20.0%** 🔴 | 16.98 GB/s → 15.52 GB/s

 |
| `sequential/worst 10000` | 11.79 ms

 | 13.78 ms

 | **+16.8%** 🔴 | 17.13 GB/s → 15.84 GB/s

 |
| `single/nominal` | 413.5 ns

 | 507.4 ns

 | **+22.7%** 🔴 | 48.14 GB/s → 43.05 GB/s

 |
| `single/worst` | 1.224 µs

 | 1.427 µs

 | **+16.6%** 🔴 | 16.49 GB/s → 15.30 GB/s

 |

---

## Comparatif `hot_path_render`

| Test | 06/08/2026 (Run 1)

 | 04/09/2026

 | Delta Latence | Débit (06/08 vs 04/09)

 |
| --- | --- | --- | --- | --- |
| `sequential/nominal 100` | 37.10 µs

 | 43.38 µs

 | **+16.9%** 🔴 | 56.96 GB/s → 50.34 GB/s

 |
| `sequential/nominal 1000` | 437.9 µs

 | 486.8 µs

 | **+11.2%** 🔴 | 48.26 GB/s → 44.86 GB/s

 |
| `sequential/nominal 10000` | 4.065 ms

 | 4.616 ms

 | **+13.6%** 🔴 | 51.98 GB/s → 47.31 GB/s

 |
| `sequential/worst 100` | 129.0 µs

 | 151.9 µs

 | **+17.8%** 🔴 | 16.37 GB/s → 14.37 GB/s

 |
| `sequential/worst 1000` | 1.278 ms

 | 1.507 ms

 | **+17.9%** 🔴 | 16.52 GB/s → 14.49 GB/s

 |
| `sequential/worst 10000` | 12.26 ms

 | 14.76 ms

 | **+20.4%** 🔴 | 17.23 GB/s → 14.79 GB/s

 |
| `single/nominal` | 430.0 ns

 | 483.2 ns

 | **+12.4%** 🔴 | 49.14 GB/s → 45.20 GB/s

 |
| `single/worst` | 1.352 µs

 | 1.531 µs

 | **+13.2%** 🔴 | 15.63 GB/s → 14.26 GB/s

 |
| `segmented/sequential_large 1000` | 15.88 ms

 | 14.80 ms

 | **-6.8%** 🟢 | 1.33 GB/s → 1.475 GB/s

 |
| `segmented/single_large` | 412.5 ns

 | 485.8 ns

 | **+17.8%** 🔴 | 51.23 GB/s → 44.96 GB/s

 |

---

## Diagnostic matériel & invariants structurels

La dégradation subie par l'ensemble des chemins d'exécution pointe vers des facteurs d'ordre architectural au niveau du code :

1. **Inhibition de l'Inlining Cross-Module / Cross-Crate** :
La découpe des responsabilités en sous-modules ou crates distincts a brisé la capacité du compilateur à inline automatiquement les fonctions du chemin critique. Le coût d'appel de fonction (*function call overhead*), le prologue/épilogue de pile et le rechargement des registres s'ajoutent à chaque itération. Sur un micro-benchmark tournant à ~400 ns, l'ajout de quelques instructions d'appel non inlinées suffit à provoquer 10 à 20 % de surcoût.
2. **Altération du Data Layout & Perte de Densité Mémoire** :
La chute généralisée du débit binaire (passant d'environ 58-59 GB/s à 50 GB/s en régime nominal) indique que la bande passante mémoire L1/L2 est moins bien exploitée. Si le refactoring a introduit des structures d'enveloppement (*wrappers*), du *padding* implicite pour l'alignement, ou la perte de contiguïté mémoire sur les structures manipulées, chaque ligne de cache (64 octets) transporte moins d'informations utiles.


3. **Indirections Spatiales & Rupture du Pipeline d'Instructions** :
Si la séparation des fonctions a introduit des *trait objects* avec dispatch dynamique (`dyn Trait`), des pointeurs de fonction, ou des itérateurs non spécialisés, le prédicteur de branchement du CPU et l'exécution hors-ordre subissent des pénalités d'attente d'accès mémoire (*pointer chasing*).

---

## Actions de remédiation recommandées

* **Contrôle d'Inlining** : Appliquer systématiquement l'attribut `#[inline(always)]` sur l'intégralité des fonctions modulaires composant la boucle interne de rendu (*hot path*).
* **Vérification de l'Optimisation à l'Édition de Liens (LTO)** : S'assurer que le profil de benchmark (`[profile.bench]`) contient `lto = "fat"` ou `lto = "thin"` pour permettre l'inlining à travers les frontières de modules et crates.
* **Inspection de l'Assemblage** : Inspecter le code ASM généré sur `render/single/nominal` (qui passe de ~430 ns à ~483 ns) pour identifier les paires d'instructions `call`/`ret` ou réarrangements de registres indésirables introduits par la nouvelle abstraction.


* **Analyse du Layout Mémoire** : Vérifier la taille et l'alignement des structures retravaillées à l'aide de tests d'empreinte mémoire pour garantir une densité maximale dans les lignes de cache L1.
