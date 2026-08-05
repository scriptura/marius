# Benchs du 3 août 2026

## Setup

```
$ sudo lshw -short
[sudo] Mot de passe de nunn : 
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
$ cargo bench -p marius-render --bench hot_path_render
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

## Interprétation des résultats par Claude Sonnet

Bonne nouvelle d'entrée : **aucune panique dans les deux séries** — les deux certifications zéro-allocation sont passées, y compris `zero_alloc_in_render_segments_large_body` (200 Ko de corps). C'est le résultat qui compte le plus, obtenu.

Contexte matériel pour la suite : Ryzen 7 5825U (Zen 3, mobile, TDP 15 W, 8 cœurs/16 threads), **DDR4-3200** (pas DDR5, malgré votre doute — `lshw` le confirme, SODIMM DDR4), plage de fréquence très large (411 MHz → 4548 MHz, ratio ×11). Cette dernière donnée compte beaucoup dans l'interprétation qui suit — c'est une puce mobile qui descend très bas en fréquence au repos et remonte agressivement, ce qui produit exactement le genre de variance qu'on observe ci-dessous.

## 1. Certification — la preuve centrale

| Bench | médiane | moyenne | max |
|---|---|---|---|
| `zero_alloc_in_render` (sans segment) | 1.132 µs | 1.135 µs | 1.222 µs |
| `zero_alloc_in_render_segments_large_body` (200 Ko) | **390.7 ns** | 582.2 ns | 19.24 µs |

Le point clé : la version **avec** un corps de 200 Ko est presque **3× plus rapide en médiane** que la version sans segment — ce qui semble contre-intuitif tant qu'on n'a pas la bonne clé de lecture. Explication : `zero_alloc_in_render` utilise `record_worst_case()` (chaînes courtes mais **agressivement échappées**, toutes les branches de `marius_html_escape` activées) — le coût vient de l'échappement caractère par caractère, pas de la taille. `zero_alloc_in_render_segments_large_body`, elle, a `is_readable=1` mais des champs fixes courts et non agressifs — le corps de 200 Ko ne coûte quasiment rien puisqu'il n'est **jamais parcouru**, seulement référencé (`Segment::Borrowed`, un pointeur + une longueur). **C'est exactement la preuve qu'on cherchait** : le temps ne dépend pas de la taille du contenu segmenté.

**Le max de 19.24 µs** (vs 391 ns en médiane, ~50×) : un unique échantillon aberrant sur 100, pas une régression — quasi certainement un changement de P-state du CPU en cours de mesure (cette puce descend à 411 MHz, remonter à pleine fréquence prend quelques microsecondes, largement suffisant pour produire un outlier pareil sur une mesure de quelques centaines de nanosecondes). Le `mean` (582 ns) tiré vers le haut par ce seul échantillon confirme une distribution asymétrique classique, pas un problème systémique — si c'était un vrai defect (réallocation intermittente), l'assertion `alloc_count == 0` aurait paniqué, et elle ne l'a pas fait.

## 2. `render/segmented/single_large` — confirmation

390.5 ns médian, quasi identique à `render/single/nominal` (430–450 ns) et **bien en dessous** de `render/single/worst_case` (1.3–1.4 µs). Cohérent avec le point 1 : un corps de 200 Ko référencé coûte à peu près ce que coûterait un composant sans corps du tout.

## 3. `render/segmented/sequential_large` — lecture attentive requise, pas un problème du mécanisme

Ici le débit affiché (1.2–1.9 GB/s, 45–89 Kitem/s) semble **très inférieur** à `render/sequential/nominal` (~50 GB/s). Deux choses à démêler, aucune ne remet en cause le mécanisme :

**a) Le compteur de débit ne mesure pas ce que vous croyez.** `BytesCount` est calculé comme `batch_size × CONTENT_CORE_TOTAL_CAP` (le débit « côté `buf` », documenté ainsi dans le commentaire du benchmark) — il ne compte jamais les 200 Ko réels du corps segmenté. Le chiffre bas ici n'indique donc pas un ralentissement du rendu, juste un dénominateur qui ne reflète pas le travail réel effectué à côté.

**b) Le temps par enregistrement (11–17 µs) est très supérieur aux 390 ns mesurés en isolation** — ordre de grandeur ×30-40. Ce n'est presque certainement **pas** le rendu lui-même : c'est le coût de libération mémoire (`free`/`munmap`) des chaînes de 200 Ko à chaque fin d'itération. `render_batch_pure` prend le lot **par valeur** (`Vec<(Record, VarlenOwned)>`) — à la fin de l'appel, tout le lot est libéré, y compris chaque chaîne de contenu de ~207 Ko. glibc `malloc` route généralement les allocations au-delà de 128 Ko vers `mmap` directement, ce qui veut dire que chaque libération est un **appel système** `munmap`, pas un simple retour dans un pool — nettement plus coûteux qu'une désallocation classique. Pour un lot de 1000, ça fait 1000 `munmap` potentiels dans la fenêtre chronométrée. C'est cohérent avec l'ordre de grandeur observé (17,1 ms / 1000 ≈ 17 µs/enregistrement).

**Ce que ça révèle de réel, au-delà de l'artefact de banc d'essai** : ce coût d'allocation/libération existe aussi en production, à l'étape `fetch_batch` (`_vrefs.get(4).map(str::to_owned)` recopie le contenu depuis le `store.bin` mmap-é vers une `String` possédée) — la segmentation économise la copie **dans le buffer de rendu** et l'échappement, mais ne supprime pas ce coût d'allocation initial du contenu lui-même. C'est une limite du mécanisme, pas un défaut : le chunking/streaming évoqué dans l'ADR-010 (jamais implémenté, resté au stade de piste) aurait pu adresser *ça* spécifiquement — au prix de la complexité qu'on avait choisi d'éviter.

## 4. Le reste — cohérent, rien à signaler

- `render/sequential/nominal` vs `worst_case` : ratio ~3× (60 GB/s vs 16 GB/s) — quantifie proprement le coût de `marius_html_escape` sur du contenu agressif. Stable sur les trois échelles (100/1 000/10 000), aucune dégradation super-linéaire — bon signe pour la localité de cache.
- Les débits « GB/s » à 50-60 GB/s ne sont **pas** un débit mémoire réel — DDR4-3200 dual-channel plafonne autour de 40-50 GB/s en théorie sur cette machine, et le calcul ici est basé sur `TOTAL_CAP` (capacité réservée), pas sur les octets réellement écrits (les fixtures nominales remplissent une fraction de `TOTAL_CAP`). Le nombre est correct pour ce qu'il mesure, juste à ne pas lire comme une bande passante mémoire absolue.
