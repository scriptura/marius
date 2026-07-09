# Spécification de la pile technique et cartographie réactive

## 1. Alignement Système (Le "Pourquoi")

Ce document cartographie l'infrastructure technique du moteur Marius. Le système rejette les architectures web classiques au profit d'une approche **Data-Oriented Design (DOD)** et **Ahead-Of-Time (AOT)**.

- **La donnée ordonne la projection :** Le serveur ne calcule rien à la demande, il projette de l'état SQL sous forme d'artéfacts physiques (HTML).

- **Déterminisme absolu :** La latence sur le chemin de lecture est aplatie au niveau du noyau, éliminant les couches applicatives et la gestion dynamique de la mémoire au runtime.

- **Zéro Indirection :** Suppression des ORM et des sérialisations JSON. Les octets transitent de PostgreSQL vers le code machine Rust compilé, puis sont figés sur le disque.

---

## 2. Cartographie de la Pile (Le "Quoi")

Chaque brique est sélectionnée pour sa capacité à s'effacer au runtime et à respecter l'attitude `no_std` du Core.

| Composant     | Technologie     | Rôle Système                                                                                               |
| ------------- | --------------- | ---------------------------------------------------------------------------------------------------------- |
| **Socle CPU** | **Rust (LLVM)** | Garantit la sécurité mémoire à la compilation, l'absence de Garbage Collector et un footprint RAM minimal. |

|
| **Shell I/O** | **Tokio** | Ordonnanceur multi-thread (Work-Stealing) gérant les entrées/sorties non-bloquantes du pipeline réactif.

|
| **Délivrance** | **Axum + Tower** | Routeur HTTP apatride. Il extrait l'identité pour mapper le chemin VFS et commande le streaming via `sendfile(2)`.

|
| **Rendu (AOT)** | **Préprocesseur `.marius**`| Script`build.rs` transformant les templates en fonctions Rust natives (`push_str`/`write_fmt`). Élimine Maud et tout parsing au runtime.

|
| **Data Driver** | **SQLx** | Validation des requêtes SQL contre le schéma de production _au moment de la compilation_.

|
| **Moteur d'État** | **PostgreSQL** | Source de vérité unique. Pilote le rebond réactif via ses politiques RLS et le protocole natif `LISTEN/NOTIFY`.

|
| **Protocole Client** | **HTMX** | Traite le navigateur comme un terminal d'affichage en permutant des fragments de DOM injectés en AOT.

|
| **Assets Pipeline** | **`build.rs` + `minify-js**` | Minification, hachage et inclusion statique des scripts pour un cache navigateur immuable.

|

---

## 3. Topologie des Pipelines (Le "Comment")

L'architecture sépare strictement le chemin de lecture (Read Path, critique en latence) du chemin d'écriture (Write Path, critique en débit).

### A. Le Chemin de Lecture (Read Path - $O(1)$)

1. **Requête HTTP :** Axum intercepte la demande et extrait les cookies/tokens de session.
2. **Mapping VFS :** L'identité de l'utilisateur détermine le sous-répertoire de rôle (sécurité RLS résolue en amont).
3. **Passthrough Kernel :** Axum pointe vers l'artéfact HTML sur le disque et appelle `sendfile(2)`. Le noyau Linux pousse les pages du cache vers la socket réseau sans remonter dans l'espace utilisateur Rust.

### B. Le Chemin de Mutation (Write Path & Projection Réactive)

Le problème d'amplification d'écriture (_Write Amplification_) est résolu par le découplage temporel du pattern **Collector/Dispatcher**.

```
[PostgreSQL Mutation]
         │
         ▼ (pg_notify + ID)[cite: 3]
[Collector (Bit-Vector Lock-Free)] -> Consolidation / Dédoublonnement[cite: 3]
         │
         ▼ (Tick temporel / Flush)[cite: 3]
[Dispatcher (Batch Extraction)] -> Requête SQLx groupée[cite: 3]
         │
         ▼ (Parallélisation Rayon/Tokio)[cite: 3]
[Code généré par .marius] -> push_str() synchrone[cite: 3]
         │
         ▼ (Écriture préemptive)
   [HTML sur Disque]

```

---

## 4. Invariants de l'Interface Client

Les micro-interactions locales sans persistance (ex: ouvertures de menus, onglets) sont isolées du serveur.

- **Règle :** Utilisation exclusive de Vanilla JS ou d'attributs HTML natifs (`aria-expanded`, `<details>`).

- **Cycle de Vie HTMX :** Les scripts réactifs s'enregistrent sur le hook `htmx.onLoad`. Cela garantit que tout nouveau fragment HTML injecté par le pipeline d'écriture hérite immédiatement de ses comportements sans réévaluation globale du script.
