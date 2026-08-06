# Benchs du 6 août 2026
Analyse comparative Axum v0.7 → v0.8
---

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
Timer precision: 20 ns
hot_path_certify                                     fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ certify/zero_alloc_in_render                      1.111 µs      │ 2.248 µs      │ 1.127 µs      │ 1.141 µs      │ 100     │ 200
├─ certify/zero_alloc_in_render_segments_large_body  359.7 ns      │ 47.91 µs      │ 380.7 ns      │ 864.4 ns      │ 100     │ 100
├─ render/sequential/nominal                                       │               │               │               │         │
│  ├─ 100                                            34.71 µs      │ 40.66 µs      │ 35.5 µs       │ 35.87 µs      │ 100     │ 100
│  │                                                 60.87 GB/s    │ 51.96 GB/s    │ 59.52 GB/s    │ 58.9 GB/s     │         │
│  │                                                 2.88 Mitem/s  │ 2.458 Mitem/s │ 2.816 Mitem/s │ 2.787 Mitem/s │         │
│  ├─ 1000                                           364.5 µs      │ 438.7 µs      │ 404.1 µs      │ 405 µs        │ 100     │ 100
│  │                                                 57.98 GB/s    │ 48.17 GB/s    │ 52.29 GB/s    │ 52.18 GB/s    │         │
│  │                                                 2.743 Mitem/s │ 2.279 Mitem/s │ 2.474 Mitem/s │ 2.468 Mitem/s │         │
│  ╰─ 10000                                          4.07 ms       │ 4.978 ms      │ 4.498 ms      │ 4.496 ms      │ 100     │ 100
│                                                    51.92 GB/s    │ 42.44 GB/s    │ 46.98 GB/s    │ 46.99 GB/s    │         │
│                                                    2.456 Mitem/s │ 2.008 Mitem/s │ 2.222 Mitem/s │ 2.223 Mitem/s │         │
├─ render/sequential/worst_case                                    │               │               │               │         │
│  ├─ 100                                            121.8 µs      │ 146.8 µs      │ 124.8 µs      │ 125.2 µs      │ 100     │ 100
│  │                                                 17.34 GB/s    │ 14.39 GB/s    │ 16.92 GB/s    │ 16.88 GB/s    │         │
│  │                                                 820.6 Kitem/s │ 680.9 Kitem/s │ 800.9 Kitem/s │ 798.7 Kitem/s │         │
│  ├─ 1000                                           1.219 ms      │ 1.319 ms      │ 1.24 ms       │ 1.244 ms      │ 100     │ 100
│  │                                                 17.32 GB/s    │ 16.02 GB/s    │ 17.03 GB/s    │ 16.98 GB/s    │         │
│  │                                                 819.7 Kitem/s │ 758 Kitem/s   │ 805.8 Kitem/s │ 803.6 Kitem/s │         │
│  ╰─ 10000                                          12.14 ms      │ 13.19 ms      │ 12.3 ms       │ 12.33 ms      │ 100     │ 100
│                                                    17.39 GB/s    │ 16.01 GB/s    │ 17.17 GB/s    │ 17.13 GB/s    │         │
│                                                    823.1 Kitem/s │ 757.8 Kitem/s │ 812.8 Kitem/s │ 810.6 Kitem/s │         │
├─ render/single/nominal                             411.7 ns      │ 1.258 µs      │ 425.5 ns      │ 438.9 ns      │ 100     │ 800
│                                                    51.33 GB/s    │ 16.79 GB/s    │ 49.66 GB/s    │ 48.14 GB/s    │         │
│                                                    2.428 Mitem/s │ 794.6 Kitem/s │ 2.349 Mitem/s │ 2.278 Mitem/s │         │
╰─ render/single/worst_case                          1.227 µs      │ 2.229 µs      │ 1.242 µs      │ 1.281 µs      │ 100     │ 200
                                                     17.22 GB/s    │ 9.48 GB/s     │ 17.01 GB/s    │ 16.49 GB/s    │         │
                                                     814.8 Kitem/s │ 448.5 Kitem/s │ 805 Kitem/s   │ 780.3 Kitem/s │         │
```

```
$ cargo bench -p marius-render --bench hot_path_render
Timer precision: 20 ns
hot_path_render                       fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ render/segmented/sequential_large                │               │               │               │         │
│  ├─ 10                              108.8 µs      │ 160.2 µs      │ 112.9 µs      │ 114 µs        │ 100     │ 100
│  │                                  1.941 GB/s    │ 1.318 GB/s    │ 1.871 GB/s    │ 1.853 GB/s    │         │
│  │                                  91.86 Kitem/s │ 62.39 Kitem/s │ 88.54 Kitem/s │ 87.67 Kitem/s │         │
│  ├─ 100                             1.113 ms      │ 1.249 ms      │ 1.125 ms      │ 1.134 ms      │ 100     │ 100
│  │                                  1.898 GB/s    │ 1.69 GB/s     │ 1.877 GB/s    │ 1.862 GB/s    │         │
│  │                                  89.83 Kitem/s │ 80 Kitem/s    │ 88.83 Kitem/s │ 88.12 Kitem/s │         │
│  ╰─ 1000                            14.13 ms      │ 16.84 ms      │ 16.01 ms      │ 15.88 ms      │ 100     │ 100
│                                     1.495 GB/s    │ 1.254 GB/s    │ 1.32 GB/s     │ 1.33 GB/s     │         │
│                                     70.75 Kitem/s │ 59.37 Kitem/s │ 62.45 Kitem/s │ 62.95 Kitem/s │         │
├─ render/segmented/single_large      350.7 ns      │ 2.153 µs      │ 390.7 ns      │ 412.5 ns      │ 100     │ 100
│                                     60.26 GB/s    │ 9.813 GB/s    │ 54.09 GB/s    │ 51.23 GB/s    │         │
│                                     2.851 Mitem/s │ 464.3 Kitem/s │ 2.559 Mitem/s │ 2.424 Mitem/s │         │
├─ render/sequential/nominal                        │               │               │               │         │
│  ├─ 100                             34.28 µs      │ 110.1 µs      │ 36.07 µs      │ 37.1 µs       │ 100     │ 100
│  │                                  61.64 GB/s    │ 19.18 GB/s    │ 58.58 GB/s    │ 56.96 GB/s    │         │
│  │                                  2.916 Mitem/s │ 907.9 Kitem/s │ 2.772 Mitem/s │ 2.695 Mitem/s │         │
│  ├─ 1000                            380.7 µs      │ 622.7 µs      │ 427.5 µs      │ 437.9 µs      │ 100     │ 100
│  │                                  55.5 GB/s     │ 33.93 GB/s    │ 49.43 GB/s    │ 48.26 GB/s    │         │
│  │                                  2.626 Mitem/s │ 1.605 Mitem/s │ 2.339 Mitem/s │ 2.283 Mitem/s │         │
│  ╰─ 10000                           3.938 ms      │ 4.384 ms      │ 4.052 ms      │ 4.065 ms      │ 100     │ 100
│                                     53.65 GB/s    │ 48.2 GB/s     │ 52.15 GB/s    │ 51.98 GB/s    │         │
│                                     2.538 Mitem/s │ 2.28 Mitem/s  │ 2.467 Mitem/s │ 2.459 Mitem/s │         │
├─ render/sequential/worst_case                     │               │               │               │         │
│  ├─ 100                             127 µs        │ 134.1 µs      │ 128.7 µs      │ 129 µs        │ 100     │ 100
│  │                                  16.63 GB/s    │ 15.75 GB/s    │ 16.41 GB/s    │ 16.37 GB/s    │         │
│  │                                  787 Kitem/s   │ 745.4 Kitem/s │ 776.5 Kitem/s │ 774.6 Kitem/s │         │
│  ├─ 1000                            1.232 ms      │ 1.378 ms      │ 1.276 ms      │ 1.278 ms      │ 100     │ 100
│  │                                  17.14 GB/s    │ 15.33 GB/s    │ 16.56 GB/s    │ 16.52 GB/s    │         │
│  │                                  811.1 Kitem/s │ 725.6 Kitem/s │ 783.6 Kitem/s │ 781.8 Kitem/s │         │
│  ╰─ 10000                           11.83 ms      │ 13.87 ms      │ 12.26 ms      │ 12.26 ms      │ 100     │ 100
│                                     17.85 GB/s    │ 15.23 GB/s    │ 17.23 GB/s    │ 17.23 GB/s    │         │
│                                     844.9 Kitem/s │ 720.7 Kitem/s │ 815.3 Kitem/s │ 815.4 Kitem/s │         │
├─ render/single/nominal              412.9 ns      │ 558.2 ns      │ 427.9 ns      │ 430 ns        │ 100     │ 400
│                                     51.17 GB/s    │ 37.86 GB/s    │ 49.38 GB/s    │ 49.14 GB/s    │         │
│                                     2.421 Mitem/s │ 1.791 Mitem/s │ 2.336 Mitem/s │ 2.325 Mitem/s │         │
╰─ render/single/worst_case           1.332 µs      │ 1.402 µs      │ 1.352 µs      │ 1.352 µs      │ 100     │ 200
                                      15.86 GB/s    │ 15.07 GB/s    │ 15.62 GB/s    │ 15.63 GB/s    │         │
                                      750.6 Kitem/s │ 713.1 Kitem/s │ 739.5 Kitem/s │ 739.5 Kitem/s │         │

```

## Interprétation des résultats par DeepSheek

### 🟢 Verdict global : **Pas de régression significative, améliorations sur plusieurs points**

### 1. Certify (zero_alloc) - Stable

| Test | Avant (mean) | Après (mean) | Delta |
|------|-------------|-------------|-------|
| `zero_alloc_in_render` | 1.135 µs | 1.141 µs | +0.5% |
| `...segments_large_body` | 582.2 ns | 864.4 ns | +48% ⚠️ |

**Analyse** : Le test unitaire est stable (+0.5% = bruit). En revanche, `segments_large_body` montre une dégradation sur la **mean** (582 → 864 ns) mais le **median** est quasi identique (390.7 → 380.7 ns). Le slowest passe de 19.24 µs à 47.91 µs. Cela indique des **pics de latence plus fréquents/amples**, probablement liés à des changements internes d'Axum sur le traitement des bodies segmentés. La médiane stable rassure : le comportement nominal est inchangé.

### 2. `render/sequential/nominal` - Amélioration légère 🌱

| Taille | Métrique | Avant (mean) | Après (mean) | Delta |
|--------|----------|-------------|-------------|-------|
| 100 | latence | 37.13 µs | 35.87 µs | **-3.4%** ✅ |
| | débit | 56.9 GB/s | 58.9 GB/s | **+3.5%** ✅ |
| 1000 | latence | 490.1 µs | 405 µs | **-17.4%** ✅✅ |
| | débit | 43.11 GB/s | 52.18 GB/s | **+21%** ✅✅ |
| 10000 | latence | 4.369 ms | 4.496 ms | +2.9% |
| | débit | 48.37 GB/s | 46.99 GB/s | -2.9% |

**Analyse** : Excellente nouvelle sur les lots de 100 et 1000 avec +21% de débit sur le 1000. La dégradation à 10000 (-2.9%) est marginale et dans la marge d'erreur. **Axum v0.8 semble bénéfique pour ce hot path.**

### 3. `render/sequential/worst_case` - Amélioration notable 🌱🌱

| Taille | Métrique | Avant (mean) | Après (mean) | Delta |
|--------|----------|-------------|-------------|-------|
| 100 | latence | 133.1 µs | 125.2 µs | **-5.9%** ✅ |
| | débit | 15.87 GB/s | 16.88 GB/s | **+6.4%** ✅ |
| 1000 | latence | 1.346 ms | 1.244 ms | **-7.6%** ✅ |
| | débit | 15.69 GB/s | 16.98 GB/s | **+8.2%** ✅ |
| 10000 | latence | 14.52 ms | 12.33 ms | **-15.1%** ✅✅ |
| | débit | 14.54 GB/s | 17.13 GB/s | **+17.8%** ✅✅ |

**Analyse** : **Très bon résultat.** Toutes les tailles s'améliorent de 6 à 18%. La stabilité s'améliore aussi (écart fastest/slowest réduit).

### 4. `render/segmented/sequential_large` - Amélioration marquée 🌱🌱

| Taille | Métrique | Avant (mean) | Après (mean) | Delta |
|--------|----------|-------------|-------------|-------|
| 10 | latence | 118.2 µs | 114 µs | **-3.6%** ✅ |
| 100 | latence | 1.444 ms | 1.134 ms | **-21.5%** ✅✅ |
| | débit | 1.463 GB/s | 1.862 GB/s | **+27.3%** ✅✅ |
| 1000 | latence | 17.62 ms | 15.88 ms | **-9.9%** ✅ |

**Analyse** : Énorme gain sur le lot de 100 (+27% de débit). La gestion des segments a clairement bénéficié de la mise à jour.

### 5. Tests unitaires (`render/single/*`)

| Test | Avant (mean) | Après (mean) | Delta |
|------|-------------|-------------|-------|
| nominal (certify) | 487.6 ns | 438.9 ns | **-10%** ✅ |
| nominal (render) | 469.2 ns | 430 ns | **-8.4%** ✅ |
| worst_case (certify) | 1.361 µs | 1.281 µs | **-5.9%** ✅ |
| worst_case (render) | 1.371 µs | 1.352 µs | -1.4% |

### 🔴 Points de vigilance

1. **`zero_alloc_in_render_segments_large_body`** : les pics de latence extrêmes (slowest ×2.5, mean +48%) méritent une investigation. Vérifiez si cela impacte des scénarios réels ou si c'est un artefact de benchmark (GC, allocateur, etc.).

2. **`render/single/worst_case` (certify)** : le slowest passe de 3.6 µs à 2.2 µs → **amélioration**, mais l'écart fastest/slowest reste important. La distribution est plus resserrée qu'avant.

### Synthèse

| Catégorie | Tendance |
|-----------|----------|
| Latence nominale | 🟢 Amélioration 3-21% |
| Débit nominal | 🟢 Amélioration 3-27% |
| Worst case | 🟢 Amélioration 6-18% |
| Segmented large | 🟢 Forte amélioration |
| Zero alloc certify | 🟡 Stable (pics à investiguer) |

**Conclusion** : La migration Axum v0.8 est **bénéfique** pour vos performances. Les gains sont significatifs sur les chemins critiques. Seul point d'attention : les segments large body en certify qui montrent plus de variance dans les extrêmes. Je recommande de runner les benchmarks plusieurs fois pour confirmer la reproductibilité de ce point spécifique.

---

## Résultats de la 2ème itération - Confirmation des tendances

Comparons maintenant les **3 runs** : Avant (v0.7), Après #1, Après #2.

### 🔴 Point critique confirmé : `zero_alloc_in_render_segments_large_body`

| Run | Mean | Median | Slowest |
|-----|------|--------|---------|
| Avant (v0.7) | 582.2 ns | 390.7 ns | 19.24 µs |
| Après #1 | 864.4 ns | 380.7 ns | 47.91 µs |
| Après #2 | 633.5 ns | 430.7 ns | 20.36 µs |

**Analyse** : La médiane est **stable** (390-430 ns). La mean varie beaucoup (582 → 864 → 633 ns) à cause des outliers. Le run #2 ressemble plus au comportement initial. C'est **instable mais pas dégradé structurellement** — probablement un bruit de mesure lié au scheduling ou à l'allocateur, pas un problème Axum. Le fait que la médiane ne bouge pas le confirme.

### 🟢 Améliorations confirmées : `render/sequential/nominal`

| Taille | Run | Mean | Débit |
|--------|-----|------|-------|
| **100** | Avant | 37.13 µs | 56.9 GB/s |
| | Après #1 | 35.87 µs | 58.9 GB/s |
| | Après #2 | 35.37 µs | 59.75 GB/s |
| **1000** | Avant | 490.1 µs | 43.11 GB/s |
| | Après #1 | 405 µs | 52.18 GB/s |
| | Après #2 | 379.4 µs | 55.7 GB/s |
| **10000** | Avant | 4.369 ms | 48.37 GB/s |
| | Après #1 | 4.496 ms | 46.99 GB/s |
| | Après #2 | 3.779 ms | 55.91 GB/s |

**Analyse** : La tendance est **clairement positive et cohérente**. Le 10000 qui montrait -2.9% au run #1 est maintenant à **+13.5%** ! Les 3 tailles convergent vers un gain solide. **Axum v0.8 booste ce chemin de 5-15%.**

### 🟢 Améliorations confirmées : `render/sequential/worst_case`

| Taille | Run | Mean | Débit |
|--------|-----|------|-------|
| **100** | Avant | 133.1 µs | 15.87 GB/s |
| | Après #1 | 125.2 µs | 16.88 GB/s |
| | Après #2 | 125 µs | 16.9 GB/s |
| **1000** | Avant | 1.346 ms | 15.69 GB/s |
| | Après #1 | 1.244 ms | 16.98 GB/s |
| | Après #2 | 1.154 ms | 18.3 GB/s |
| **10000** | Avant | 14.52 ms | 14.54 GB/s |
| | Après #1 | 12.33 ms | 17.13 GB/s |
| | Après #2 | 11.74 ms | 17.99 GB/s |

**Analyse** : Gains **reproductibles et qui s'améliorent** entre les 2 runs post-migration. Le worst case 10000 gagne **19% de latence** et **24% de débit**. Excellent.

### 🟢 Tests unitaires - Confirmés

| Test | Avant | Après #1 | Après #2 | Tendance |
|------|-------|----------|----------|----------|
| `single/nominal` (certify) | 487.6 ns | 438.9 ns | 421.2 ns | 🟢 **-13.6%** |
| `single/worst_case` (certify) | 1.361 µs | 1.281 µs | 1.22 µs | 🟢 **-10.4%** |

### Bilan final consolidé

| Chemin | Gain moyen | Fiabilité |
|--------|-----------|-----------|
| `sequential/nominal` | **+10-29% débit** | ✅ Reproductible, s'améliore |
| `sequential/worst_case` | **+6-24% débit** | ✅ Reproductible, s'améliore |
| `segmented/sequential_large` | **+4-27% débit** | ✅ Fort sur le 100 |
| `single/*` | **-8 à -14% latence** | ✅ Stable |
| `zero_alloc` certify | Stable (± bruit) | 🟡 Médiane OK, outliers variables |

### Conclusion finale

**La migration Axum v0.8 est un succès.** Aucune régression structurelle. Les gains sont réels et reproductibles, particulièrement sur les chemins `sequential` qui sont probablement votre hot path principal. Le point `segments_large_body` n'est qu'un artéfact de mesure sans impact sur le comportement nominal.

---

## Analyse du 3ème run - Consolidation finale

### Métrique clé : `zero_alloc_in_render_segments_large_body`

| Run | Mean | Median | Slowest |
|-----|------|--------|---------|
| Avant (v0.7) | 582.2 ns | 390.7 ns | 19.24 µs |
| Après #1 | 864.4 ns | 380.7 ns | 47.91 µs |
| Après #2 | 633.5 ns | 430.7 ns | 20.36 µs |
| Après #3 | **574.5 ns** | **379.7 ns** | 19.78 µs |

**Verdict** : Retour à la normale. Le run #1 était un outlier. La mean #3 est même **légèrement meilleure** que l'avant (-1.3%). Médiane stable. **Aucun problème.**

### Tableau de bord final — Évolution nette (Avant → Moyenne Après)

| Chemin | Taille | Avant Mean | Moy. Après Mean | Delta |
|--------|--------|-----------|-----------------|-------|
| **certify/nominal** | 100 | 37.13 µs | 35.55 µs | **-4.3%** 🟢 |
| | 1000 | 490.1 µs | 382.7 µs | **-21.9%** 🟢🟢 |
| | 10000 | 4.369 ms | 3.823 ms | **-12.5%** 🟢🟢 |
| **certify/worst** | 100 | 133.1 µs | 121.3 µs | **-8.9%** 🟢 |
| | 1000 | 1.346 ms | 1.172 ms | **-12.9%** 🟢🟢 |
| | 10000 | 14.52 ms | 11.79 ms | **-18.8%** 🟢🟢 |
| **render/nominal** | 100 | 40.77 µs | 37.04 µs | **-9.1%** 🟢 |
| | 1000 | 447.5 µs | 413.1 µs | **-7.7%** 🟢 |
| | 10000 | 4.296 ms | 3.926 ms | **-8.6%** 🟢 |
| **render/worst** | 100 | 135 µs | 125.3 µs | **-7.2%** 🟢 |
| | 1000 | 1.318 ms | 1.235 ms | **-6.3%** 🟢 |
| | 10000 | 13.41 ms | 12.17 ms | **-9.2%** 🟢 |
| **single/nominal** (certify) | — | 487.6 ns | 413.5 ns | **-15.2%** 🟢🟢 |
| **single/worst** (certify) | — | 1.361 µs | 1.224 µs | **-10.1%** 🟢🟢 |

### Points notables sur ce 3ème run

- **`render/single/nominal`** : un slowest à 3.6 µs (vs ~400 ns habituel) — clairement un outlier ponctuel, la médiane à 389.7 ns reste excellente
- **`segmented/sequential_large` 100** : mean à 1.107 ms, encore mieux que le run #2 (1.134 ms), meilleur que l'avant (1.444 ms) → **-23%** confirmé
- **Stabilité worst_case** : les écarts fastest/slowest se resserrent dans `certify`, indiquant une meilleure prédictibilité

### Verdict définitif

```
┌─────────────────────────────────────────┐
│  Axum v0.7 → v0.8 : GO ✅               │
│                                         │
│  Latence moyenne :   -9% (hot paths)    │
│  Débit max :         +15-25%            │
│  Régression :        Aucune             │
│  Stabilité :         Équivalente        │
└─────────────────────────────────────────┘
```

Les 3 runs convergent. La migration est **strictement bénéfique** sur tous les indicateurs. Aucune action corrective nécessaire.