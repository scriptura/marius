# Note d'enquête — Projet Marius

**Objet :** Divergences documentaires et faiblesse du mécanisme de notifications
**Date :** 23 août 2026
**Destinataire :** Équipe Marius
**Source :** Confrontation des documents fondateurs, des ADRs, des guides, et du code source

---

## 1. Divergences documentaires constatées

### 1.1. La nature de l'« Extraction Data » (étape 5 du pipeline)

**Documents concernés :**
- `docs/manifestos/manifest-reactive-projection.md` (Manifeste)
- `docs/guides/runtime-lifecycle-guide.md` (Guide runtime)
- `docs/architecture/runtime-data-flow/runtime-data-flow-invariants.md` (Invariants)
- `crates/shell/render/src/dispatcher.rs` (Code)
- `crates/shell/render/src/bin/dump.rs` (Code)

**Constat :**

Le Manifeste a fait l'objet de **deux révisions** (28 juin et 7 juillet 2026) concernant la provenance des données lors de la phase de régénération :

- **Version initiale (25 mars) :** Le pipeline lisait un instantané local `store.bin` via `mmap`, sans requête SQL sur le chemin réactif.
- **Révision du 28 juin :** Le pipeline interrogeait PostgreSQL directement (`fetch_batch`) sur le delta du tick. `store.bin` n'était jamais lu à cette étape.
- **Révision du 7 juillet :** Correction de la révision précédente. Le chemin chaud HTTP reste bien sans SQL (lecture `pread` du pack déjà rendu), mais le chemin de régénération appelle `P::fetch_batch(pool, ids)`, une requête PostgreSQL live.

**Or, le document `runtime-data-flow-invariants.md` contredit cette version finale :**

| Invariant | Affirmation |
|-----------|-------------|
| Invariant 1 | `store.bin` est la seule source de vérité de `fetch_batch` |
| Invariant 2 | `fetch_batch` ne contacte jamais PostgreSQL |
| Invariant 3 | `pack.bin` est toujours dérivé de `store.bin`, jamais directement de PostgreSQL |
| Invariant 5 | `regenerate_and_swap` ne parle jamais à PostgreSQL |
| Invariant 6 | `fetch_from_pg` n'est appelé que par `dump_table` et `ingest_and_swap` |

**Le code de `dispatcher.rs` confirme les invariants, pas le Manifeste :**

```rust
// Étage 1 : ingestion DOD — fetch_from_pg → store.bin
if let Err(e) = ingest_and_swap::<P>(&self.pool, &ids, &self.io_semaphore).await { ... }

// Étage 2 : régénération — lit store.bin, jamais PostgreSQL
if let Err(e) = regenerate_and_swap::<P>(...).await { ... }
```

**Impact :**
Le Manifeste, dans sa version révisée du 7 juillet, affirme que `fetch_batch` interroge PostgreSQL. Les Invariants et le code disent le contraire : `fetch_batch` lit `store.bin`, et c'est `ingest_and_swap` (appelé avant) qui interroge PostgreSQL.

**Recommandation :**
Mettre à jour le Manifeste pour refléter fidèlement la séquence `ingest_and_swap` (SQL) → `store.bin` → `fetch_batch` (lecture locale) → `regenerate_and_swap`. La distinction entre « chemin chaud HTTP », « chemin de régénération » et « chemin d'ingestion » devrait être clarifiée.

---

### 1.2. Le Guide runtime n'est pas à jour

**Documents concernés :**
- `docs/guides/runtime-lifecycle-guide.md` (Guide runtime)
- `docs/architecture/runtime-data-flow/runtime-data-flow-invariants.md` (Invariants)
- `crates/shell/render/src/dispatcher.rs` (Code)

**Constat :**

Le Guide runtime, qui se présente comme la référence pour comprendre le cycle de vie des artefacts, contient une affirmation **contredite par les Invariants et le code**.

Dans son tableau récapitulatif des trois artefacts (§1), il indique :

> **`{table}.bin` (le pack HTML)** est produit par `regenerate_and_swap`, et son contenu est :
> *« Fragments HTML déjà rendus, indexés par `(offset, len)` — c'est **ce que `handlers.rs` sert par `pread`** »*

Puis, plus loin (§1, point de vigilance) :

> *« **Confusion fréquente, à ne pas reproduire** : `regenerate_and_swap` **n'interroge jamais `{table}_store.bin`** — il exécute `P::fetch_batch(pool, ids)`, une requête PostgreSQL live (`batch_renderer.rs`, en-tête : « distinct du store.bin »). Les deux artefacts `.bin` n'ont **aucune** dépendance de lecture entre eux ; `store.bin` sert exclusivement `marius-dump`/`marius-verify`, jamais le chemin de régénération du pack HTML. »*

**Or, les Invariants du runtime (plus récents) affirment :**

| Invariant | Affirmation |
|-----------|-------------|
| Invariant 1 | `store.bin` est la seule source de vérité de `fetch_batch` |
| Invariant 2 | `fetch_batch` ne contacte jamais PostgreSQL |
| Invariant 3 | `pack.bin` est toujours dérivé de `store.bin`, jamais directement de PostgreSQL |
| Invariant 5 | `regenerate_and_swap` ne parle jamais à PostgreSQL |

**Et le code de `dispatcher.rs` le confirme :**

```rust
// Étage 1 : ingest_and_swap — fetch_from_pg → store.bin
// Étage 2 : regenerate_and_swap — lit store.bin, jamais PostgreSQL
```

**Impact :**
Le Guide runtime contient une **erreur factuelle** : il affirme que `regenerate_and_swap` interroge PostgreSQL directement, alors que le code et les Invariants montrent qu'il lit `store.bin`, après que `ingest_and_swap` a mis ce dernier à jour.

C'est exactement la confusion que le Guide prétendait pourtant dissiper.

**Recommandation :**
Mettre à jour le Guide runtime pour refléter la séquence correcte :
1. `ingest_and_swap` interroge PostgreSQL (`fetch_from_pg`) et met à jour `store.bin`.
2. `regenerate_and_swap` lit `store.bin` (`fetch_batch`) et régénère `pack.bin`.

La phrase « `regenerate_and_swap` n'interroge jamais `{table}_store.bin` » devrait être corrigée, car c'est précisément ce qu'il fait.

---

### 1.3. La mention de `marius-dump` dans le Manifeste

**Documents concernés :**
- `docs/manifestos/manifest-reactive-projection.md` §2
- `crates/shell/render/src/bin/dump.rs`

**Constat :**

Le Manifeste révisé (7 juillet) contient ce passage :

> *« L'accès SQL réel (`marius-dump`, extraction périodique) reste, lui, correctement décrit : confiné, jamais sur le chemin chaud. »*

Le terme **« extraction périodique »** suggère une exécution automatique et régulière.

**Or, le code de `dump.rs` indique :**

```rust
//! Exécuté manuellement au déploiement : cargo run --bin marius-dump
//! Jamais par cargo build, jamais par le Dispatcher.
```

Et d'après les informations recueillies, `marius-dump` n'est exécuté que :
- au lancement initial du site,
- lors des mises à jour.

**Impact :**
L'adjectif « périodique » est trompeur. Il laisse croire qu'un mécanisme de réconciliation périodique automatique existe, alors qu'il n'en est rien.

**Recommandation :**
Remplacer « extraction périodique » par « extraction manuelle (déploiement, mises à jour) » dans le Manifeste, ou ajouter une phrase précisant qu'aucun orchestrateur n'exécute `marius-dump` en continu.

---

## 2. Faiblesse repérée sur les notifications

### 2.1. Perte silencieuse et irréversible d'un `NOTIFY`

**Documents concernés :**
- `docs/guides/runtime-lifecycle-guide.md` §3
- `crates/shell/render/src/dispatcher.rs`
- `crates/shell/render/src/bin/dump.rs`

**Constat :**

Le guide runtime documente explicitement le risque :

> *« Un `NOTIFY` émis avant que le `LISTEN` ne soit actif est perdu — redémarrer le serveur après n'y change rien, il ne rattrape jamais un événement passé. »*

**Le code du Dispatcher confirme l'absence de parade automatique :**

```rust
let mut ids = self.collector.flush();
if ids.is_empty() {
    continue;
}
```

Si le Collector est vide, le tick ne fait rien. Il se contente d'attendre le prochain signal.

**Conséquence :**

Si un `NOTIFY` est perdu (listener en panne, redéploiement, surcharge, crash), la ou les entités modifiées pendant cette fenêtre ne seront **jamais** régénérées jusqu'à ce qu'un autre signal arrive pour le même shard, ou qu'un `marius-dump` manuel soit exécuté.

**Gravité :**
- **Silencieuse :** aucune erreur, aucun log, aucun indicateur ne signale qu'une mutation n'a pas été répercutée.
- **Irréversible :** le signal perdu ne peut pas être rejoué (contrairement à une lecture du WAL).
- **Périmètre :** affecte la fraîcheur du pack HTML servi aux lecteurs, sans que l'utilisateur final puisse le détecter.

---

### 2.2. Absence de filet de sécurité automatique en production

**Documents concernés :**
- `crates/shell/render/src/bin/dump.rs`
- `docs/architecture/runtime-data-flow/runtime-data-flow-invariants.md`

**Constat :**

`marius-dump` est le seul mécanisme capable de réconcilier le pack HTML avec PostgreSQL **sans dépendre des signaux**. Il interroge la base directement, récupère tous les IDs, et régénère tout.

**Mais** son exécution est **manuelle** :
- au déploiement initial,
- lors des mises à jour.

**Il n'existe pas**, d'après les documents consultés, de mécanisme automatique (cron, systemd timer, orchestrateur) qui exécuterait `marius-dump` à intervalle régulier pour réparer les pertes de `NOTIFY`.

**Conséquence :**
En production, si un `NOTIFY` est perdu, l'écart entre la base de données et le pack HTML persiste **indéfiniment**, jusqu'à ce qu'une intervention humaine lance `marius-dump`.

---

## 3. Pistes de solutions suggérées

### 3.1. Réconciliation périodique légère

Ajouter un mécanisme de **balayage périodique** (sweep) qui ne dépend pas des signaux :

- Toutes les N secondes (ex : 30 s, 60 s), le Dispatcher pourrait interroger PostgreSQL pour vérifier si des lignes ont été modifiées depuis le dernier cycle (via `modified_at`, `xmin`, ou une table de journalisation).
- Si des écarts sont détectés, les IDs correspondants seraient injectés dans le Collector, comme s'ils provenaient d'un `NOTIFY`.

**Avantage :** Réparation automatique sans dépendre de `marius-dump`.

**Coût :** Une requête SQL légère par shard et par intervalle. Acceptable si l'intervalle est raisonnable.

---

### 3.2. Journalisation des mutations dans PostgreSQL

Créer une table de journalisation (`mutation_log`) alimentée par les triggers `AFTER INSERT/UPDATE/DELETE`, en plus du `NOTIFY`.

- Le trigger écrirait l'ID modifié dans cette table.
- Un mécanisme de réconciliation périodique lirait cette table, extrairait les IDs non traités, et les injecterait dans le Collector.
- Les IDs traités seraient purgés.

**Avantage :** Les événements ne sont plus perdus. Ils sont persistés jusqu'à leur traitement.

**Coût :** Une écriture supplémentaire par mutation (la table de journalisation), et une requête de lecture périodique.

---

### 3.3. Exécution planifiée de `marius-dump`

Si les solutions ci-dessus sont trop lourdes, la solution minimale est de **planifier** `marius-dump` :

- Ajouter un timer systemd ou un cron qui exécute `marius-dump` toutes les N minutes.
- Documenter ce timer dans le guide de déploiement.

**Avantage :** Simple à mettre en œuvre, utilise l'existant.

**Inconvénient :** La réconciliation n'est pas incrémentale. Elle régénère tout le pack, ce qui peut être coûteux si la table est volumineuse.

---

## 4. Conclusion

Le projet Marius est remarquable par sa rigueur et sa cohérence. Les divergences documentaires relevées sont mineures et faciles à corriger. La faiblesse sur les notifications, en revanche, mérite une attention sérieuse : elle introduit une **fenêtre de péremption silencieuse** du pack HTML, sans filet de sécurité automatique en production.

Les solutions proposées ci-dessus sont des pistes. L'équipe Marius est la mieux placée pour juger de celle qui correspond le mieux à sa philosophie DOD et à ses contraintes de performance.

---

# Addendum à la note d'enquête — Projet Marius

**Objet :** Faiblesse supplémentaire repérée dans le pipeline de régénération — perte des IDs en cas d'échec de l'ingestion
**Date :** 23 août 2026
**Destinataire :** Équipe Marius
**Source :** Confrontation du code source (`dispatcher.rs`, `collector.rs`) et des documents d'architecture

---

## 1. Contexte

Lors de l'enquête précédente, une faiblesse avait été repérée sur la perte des notifications `NOTIFY` en amont du Collector (listener en panne, redéploiement, etc.).

Un second problème, plus profond, a été identifié en aval du Collector : **la perte des IDs au moment du traitement**, en cas d'échec de l'ingestion PostgreSQL.

---

## 2. Le problème

### 2.1. Le Collector est un tampon destructif

Le fichier `crates/core/collector/src/collector.rs` révèle que le Collector est un pur bit-vector, sans état intermédiaire.

La méthode `flush()` vide le bit-vector **immédiatement** :

```rust
pub fn flush(&self) -> Vec<i64> {
    let mut ids = Vec::with_capacity(self.count.load(Relaxed));

    for w in 0..WORDS {
        let mut word = self.presence[w].swap(0, AcqRel);  // ← remise à zéro instantanée
        while word != 0 {
            let bit = word.trailing_zeros() as usize;
            ids.push((w * 64 + bit + 1) as i64);
            word &= word - 1;
        }
    }

    self.count.store(0, Release);  // ← compteur remis à zéro aussi
    ids
}
```

**Conséquence :** une fois `flush()` appelé, les IDs sont physiquement effacés. Il n'existe aucune copie de secours, aucune file "pending", aucun mécanisme de ré-injection.

---

### 2.2. Le Dispatcher ne récupère pas les IDs en cas d'échec

Le fichier `crates/shell/render/src/dispatcher.rs` montre le traitement suivant :

```rust
let mut ids = self.collector.flush();
if ids.is_empty() {
    continue;
}

if let Err(e) = ingest_and_swap::<P>(&self.pool, &ids, &self.io_semaphore).await {
    eprintln!(
        "[dispatcher] ingest_and_swap (\"{}\"): {e}",
        self.packfile_key
    );
    continue; // ← les IDs sont perdus ici
}
```

**Scénario de défaillance :**

1. Un `NOTIFY` est reçu, l'ID est inséré dans le Collector.
2. Le Dispatcher se réveille, appelle `flush()`, et récupère les IDs.
3. Le Collector est maintenant **vide**.
4. `ingest_and_swap` tente une requête SQL live (`fetch_from_pg`).
5. La requête échoue (PostgreSQL lent, surchargé, ou momentanément indisponible).
6. Le code affiche une erreur sur `stderr`, puis exécute `continue`.
7. **Les IDs sont définitivement perdus.** Le pack HTML restera périmé pour ces entités, jusqu'à ce qu'un autre `NOTIFY` arrive pour les mêmes entités, ou qu'un `marius-dump` manuel soit exécuté.

---

### 2.3. Absence de métrique ou d'observabilité

Le Collector possède bien un compteur `dropped` pour les IDs hors périmètre (`id > MAX`) :

```rust
pub fn dropped_total(&self) -> u64 {
    self.dropped.load(Relaxed)
}
```

**Mais il n'existe aucun compteur équivalent pour les IDs perdus par échec d'ingestion.**

Le seul signal d'erreur est un `eprintln!` dans le Dispatcher. En production, cette sortie standard n'est pas nécessairement surveillée. La perte est donc **silencieuse**.

---

2.4. Échec de l'Étage 2 (regenerate_and_swap)

Si ingest_and_swap réussit mais que regenerate_and_swap échoue, le store.bin est à jour mais le pack.bin est périmé. Les IDs ont été retirés du Collector par le flush() initial, et aucun mécanisme ne retentera la régénération.

Le système se retrouve dans un état incohérent et silencieux : la source de vérité locale est plus fraîche que le pack servi. Ce cas n'est pas couvert par les invariants actuels.

---

## 3. Pourquoi c'est important

Cette faiblesse est plus grave que la perte d'un `NOTIFY` en amont, pour deux raisons :

1. **Le signal avait été reçu.** L'ID était bien présent dans le Collector. C'est le pipeline interne de Marius qui l'a détruit avant de le traiter.

2. **La perte est silencieuse.** Aucun compteur, aucune métrique, aucun log structuré ne permet de détecter qu'un ID a été perdu en cours de traitement.

---

## 4. Pistes de solutions

### 4.1. Ré-injection des IDs en cas d'échec (solution minimale)

Dans le Dispatcher, en cas d'échec de `ingest_and_swap`, réinsérer les IDs dans le Collector :

```rust
if let Err(e) = ingest_and_swap::<P>(&self.pool, &ids, &self.io_semaphore).await {
    eprintln!("[dispatcher] ingest_and_swap (\"{}\"): {e}", self.packfile_key);
    for id in &ids {
        let _ = self.collector.insert(*id, self.config.threshold_flush);
    }
    continue;
}
```

**Avantage :** Simple à implémenter, ne change pas la structure du Collector.

**Inconvénient :** Risque de boucle infinie si l'ingestion échoue en permanence. Il faudrait un mécanisme de backoff ou un nombre maximal de tentatives.

---

### 4.2. File d'acquittement (solution plus robuste)

Modifier le Collector pour qu'il conserve les IDs "en cours" jusqu'à confirmation :

- `flush()` déplacerait les IDs vers un état "processing".
- Une méthode `ack(ids)` serait appelée par le Dispatcher après succès de l'ingestion.
- Une méthode `nack(ids)` ou `requeue(ids)` serait appelée en cas d'échec.

**Avantage :** Garantie de livraison "at-least-once".

**Inconvénient :** Nécessite de modifier le Collector, qui est actuellement un pur bit-vector lock-free. Cela introduirait une complexité supplémentaire dans le Core.

---

### 4.3. Compteur d'échecs d'ingestion (solution d'observabilité minimale)

Ajouter un compteur atomique dans le Dispatcher (ou dans une structure partagée) qui compte les échecs d'`ingest_and_swap` :

```rust
static INGEST_FAILURES: AtomicU64 = AtomicU64::new(0);
```

Ce compteur serait incrémenté à chaque `eprintln!`, et exposé via une métrique ou un endpoint de santé.

**Avantage :** Simple, ne change pas le comportement, mais rend la perte **visible**.

**Inconvénient :** Ne résout pas le problème de fond, mais permet de le détecter.

---

### 4.4. Réconciliation périodique (solution déjà évoquée dans la note principale)

Comme mentionné dans la note précédente, un mécanisme de réconciliation périodique (balayage ou `marius-dump` planifié) réparerait les pertes, qu'elles viennent de l'amont ou de l'aval.

---

## 5. Conclusion

Le Collector de Marius est remarquable par sa simplicité et son efficacité : un pur bit-vector lock-free, sans allocation, tenant dans le cache L2.

Mais cette simplicité a un coût : **le Collector ne garantit pas la livraison des IDs.** Il garantit la déduplication (`Duplicate`), il garantit le bornage (`Dropped`), mais il ne garantit pas le traitement.

Le Dispatcher, en vidant le Collector avant de confirmer l'ingestion, transforme une défaillance temporaire de PostgreSQL en une **perte définitive et silencieuse** de l'information de régénération.

C'est exactement le type de faille que les systèmes de production finissent par rencontrer, et le type de robustesse que l'équipe Marius, avec sa rigueur habituelle, saura corriger si elle le juge pertinent.

---

**Rédigé dans le cadre d'une enquête sur la robustesse du système Marius**