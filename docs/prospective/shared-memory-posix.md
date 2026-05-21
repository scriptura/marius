# Document d'Architecture Système : Couplage `POSIX shm` (Zéro-Copie)

**Projet :** Marius Phase 2  
**Objet :** Transition de la Zéro-Indirection (`pgwire`) vers la Zéro-Copie (`mmap`) via mémoire partagée.  
**Statut :** Spécification technique arrêtée.  
**Révision :** Intègre les arbitrages d'audit — slots 64B, wrap-around consommateur, attachement `inotify`, backpressure drop/resync, Ping-Pong arena, NT stores.

---

## 1. Topologie des Processus et Invariant SPSC

La communication s'établit via un segment de mémoire partagée POSIX (`/dev/shm/marius_engine`),
structuré comme un bus système **SPSC (Single-Producer Single-Consumer) lock-free**.

- **Producteur Unique (PostgreSQL BGWorker) :** L'extension `pgrx` déploie un Background Worker
  dédié. Les backends PostgreSQL (`INSERT/UPDATE`) signalent le BGWorker sans écrire directement
  dans le SHM. Le BGWorker est le seul processus autorisé à écrire dans le segment — ce qui
  élimine le _cache bouncing_ des instructions `compare_exchange` inhérent à un modèle MPSC
  (Multi-Producer).

- **Consommateur Unique (Marius Core / Collector) :** Le thread principal de l'Orchestrator Rust
  dépile les événements du ring buffer en O(1) et gère les pointeurs de libération de l'arena
  active.

- **Séquence de Boot (Ownership) :** Le BGWorker est le propriétaire matériel du segment
  (`O_CREAT | O_RDWR | O_EXCL`) et l'initialise entièrement avant de le signaler. Le processus
  Rust s'y attache via `inotify` (§3). Ce protocole résout la dépendance circulaire au démarrage :
  un attachement anticipé du côté Rust sur un segment partiellement initialisé conduirait à
  une lecture de `layout_version` indéfinie.

---

## 2. Layout Binaire et Adressage Relatif

L'adressage interne au segment proscrit les pointeurs absolus pour neutraliser l'ASLR.
Toute adresse est calculée dynamiquement : `base_ptr.add(offset)`.

Le segment contient quatre zones physiquement adjacentes.

### 2.1. Zone Meta (En-tête de validation et synchronisation)

Structure `#[repr(C)]` portant les compteurs atomiques du ring buffer et les primitives
de synchronisation des arenas :

```rust
#[repr(C)]
pub struct ShmMeta {
    pub layout_version:    u64,           // Hash DDL compilé : env!("DDL_HASH")
    pub ring_head:         AtomicU64,     // Prochain slot à écrire (producteur)
    pub ring_tail:         AtomicU64,     // Prochain slot à lire  (consommateur)
    pub arena_write_index: AtomicU8,      // Arena active en écriture : 0 ou 1
    pub arena_head:        [AtomicU32; 2],// Curseur d'écriture pour chaque arena
    pub dropped_events:    AtomicU64,     // Compteur d'événements perdus (backpressure)
    pub needs_resync:      AtomicBool,    // Drapeau : au moins un drop s'est produit
    _pad: [u8; ...],                      // Padding jusqu'à 128 octets (alignement 64B × 2)
}
```

`layout_version` : hash statique du DDL injecté à la compilation par la DB-Forge
(`env!("DDL_HASH")`). Si le Collector Rust détecte une asymétrie à l'attachement, il refuse le
`mmap`, logue une erreur fatale et se termine.

**Relation avec les `static_assertions` (risques.md §1) :** ces deux gardes sont complémentaires
et non substituables. Les `static_assertions` bloquent la _compilation_ en cas de désalignement
DDL/Rust sur la même base de code. Le `layout_version` bloque le _boot_ en cas de déploiement
partiel — binaire Rust et extension PG compilés depuis des révisions DDL différentes.
Les deux mécanismes doivent coexister.

### 2.2. Zone Primaire — Ring Buffer (Slots de 64 octets)

Contient les slots fixes du ring buffer. Chaque slot occupe exactement **64 octets**,
soit une ligne de cache L1 complète.

**Invariant anti-false-sharing :** dans un SPSC, le producteur écrit en `ring_head` pendant
que le consommateur lit en `ring_tail`. Si deux slots adjacents partagent une ligne de cache
(slots de 32 octets), le consommateur charge en L1 le slot qu'il lit _et_ le slot suivant en
cours d'écriture par le producteur sur un autre cœur. L'invalidation de cohérence de cache
résultante génère une contention inutile. Des slots de 64 octets y mettent fin
structurellement : chaque slot occupe exactement une ligne, sans chevauchement possible.

**Layout SSO du slot (64 octets) :**

```
Offset  0 : tag : u8
  — tag == 0 (Inline)  : octets 1..63 → payload inline (chaîne ≤ 63 octets)
  — tag == 1 (Arena)   : octets 1..4  → offset: u32 dans l'arena active
                         octets 5..8  → length: u32
                         octets 9..63 → padding mort
```

- **Inline (tag == 0) :** chaîne ≤ 63 octets copiée directement dans le slot. Zéro indirection.
- **Arena (tag == 1) :** l'`offset` et le `length` désignent le bloc de données dans la bump
  arena active au moment de l'écriture. Le wrap-around éventuel de la chaîne en fin d'arena
  est résolu côté consommateur (§5.2) — le slot ne stocke aucune information supplémentaire.

### 2.3. Zone Secondaire — Bump Arenas en Ping-Pong

Deux arenas circulaires de capacité égale (`arena[0]` et `arena[1]`), physiquement contiguës
dans le segment après la zone primaire. Le champ `arena_write_index` de la Meta désigne l'arena
dans laquelle le BGWorker écrit à l'instant T.

**Objectif :** découpler le cycle de vie des workers Rust — qui lisent depuis l'arena `W` figée
au moment du Tick T — du cycle de production BGWorker — qui continue d'écrire dans l'arena
`1 - W` pendant le rendu. Un Tick T+1 peut démarrer et faire basculer l'arena active sans
corrompre les données qu'un worker du Tick T est encore en train de lire dans l'arena `W`.

La rotation est gouvernée par le Collector au moment du `flush` (§5.1).

---

## 3. Boot et Attachement via `inotify`

**Contrainte :** le processus Rust ne peut pas se fier à un timing fixe pour tenter l'ouverture
du segment. Un busy-wait sur `/dev/shm/` est inacceptable (consommation CPU continue).
Un sleep arbitraire est fragile (heuristique non déterministe).

**Protocole d'attachement :**

1. Au démarrage, le processus Rust enregistre un watcher `inotify` sur `/dev/shm/`,
   filtrant l'événement `IN_CREATE`.
2. Le BGWorker crée le segment (`shm_open` avec `O_CREAT | O_RDWR | O_EXCL`), le redimensionne
   (`ftruncate`), initialise la Zone Meta entièrement, puis ferme le descripteur de fichier
   côté PG (le segment reste ouvert via le `mmap` interne du BGWorker).
3. À la réception de `IN_CREATE` pour `marius_engine`, le processus Rust ouvre le descripteur,
   appelle `mmap(PROT_READ | PROT_WRITE, MAP_SHARED)`, lit `meta.layout_version`.
4. Si le hash correspond : attachement validé, le Collector démarre.
5. Si le hash diverge : fermeture du descripteur, log d'erreur fatale
   `[BOOT] DDL hash mismatch: expected {A}, found {B}`, terminaison du processus Rust.
   Le redéploiement synchronisé des deux artefacts (extension PG + binaire Rust) est requis.

**Crash recovery :** Le BGWorker utilise `O_EXCL`. Si le segment existe déjà au démarrage
(crash précédent sans nettoyage), le BGWorker appelle `shm_unlink` sur l'ancienne ressource
avant de recréer. Il ne tente jamais d'hériter d'un segment dont les pointeurs Head/Tail
peuvent être dans un état indéterminé.

---

## 4. Traitement TOAST et Stores Non-Temporels (NT Stores)

### 4.1. Détoastage et Linéarisation

Les données longues stockées hors-ligne par le mécanisme TOAST de PostgreSQL sont intégralement
linéarisées par le BGWorker via `pg_detoast_datum`. Pour éviter les fuites mémoire (OOM) liées
au cycle de vie persistant du BGWorker, chaque itération d'écriture s'exécute dans un
`MemoryContext` éphémère (`AllocSetContextCreate`), réinitialisé (`MemoryContextReset`)
immédiatement après le transfert vers l'arena.

### 4.2. Instructions NT (_Non-Temporal Stores_)

La propriété de non-pollution du cache L1/L2 lors de la copie vers la bump arena exige l'usage
explicite d'instructions NT. Sans elles, toute écriture transite par le cache du cœur producteur
et invalide les lignes correspondantes sur les cœurs consommateurs — détruisant le bénéfice
attendu du SPSC pour les threads de rendu.

**Implémentation (x86-64, SSE2) :**

```rust
/// Copie non-temporelle vers la bump arena.
/// Préconditions : `dst` aligné sur 16 octets ; `len` multiple de 16.
/// Appeler `_mm_sfence()` après cette fonction, avant d'écrire le slot.
unsafe fn nt_copy(dst: *mut u8, src: *const u8, len: usize) {
    use std::arch::x86_64::*;
    let mut i = 0;
    while i + 16 <= len {
        let chunk = _mm_loadu_si128(src.add(i) as *const __m128i);
        _mm_stream_si128(dst.add(i) as *mut __m128i, chunk);
        i += 16;
    }
    // Traitement du reliquat (len non multiple de 16) par copie normale.
    while i < len {
        dst.add(i).write(src.add(i).read());
        i += 1;
    }
}
```

Sur AArch64, l'équivalent est `stnp` (Non-Temporal Store Pair) ou `STNT1` (SVE),
suivi d'un `dmb ish`.

**Barrière obligatoire :** après `nt_copy` et avant l'écriture du slot dans le ring buffer,
un `_mm_sfence()` (x86) / `dmb ish` (ARM) garantit l'ordre global d'écriture. Sans cette
barrière, le consommateur peut lire le slot — et donc l'offset pointant vers la bump arena —
avant que la copie NT ne soit visible en mémoire principale.

**Périmètre :** les instructions NT s'appliquent exclusivement aux copies vers la bump arena
(charges utiles longues, une seule écriture avant lecture consommateur). Les écritures dans
le ring buffer (slots de 64 octets, séquentielles et fréquentes) transitent par le cache
normal — elles doivent être visibles en L1 pour le consommateur au prochain Tick.

---

## 5. Modèle de Synchronisation Lock-Free

### 5.1. Adaptive Tick et Rotation Ping-Pong

La séquence suivante s'exécute intégralement sur le thread du Collector à chaque `flush` :

**Étape 1 — Lecture du batch**
Le Collector lit un lot de slots depuis `ring_tail` jusqu'à `ring_head`
(Ordering::Acquire sur `ring_head`). Pour chaque slot `tag == 1`, il note l'index d'arena
courant `W = arena_write_index.load(Acquire)`.

**Étape 2 — Rotation de l'arena**
Le Collector bascule l'arena active via CAS : `arena_write_index.compare_exchange(W, 1 - W,
AcqRel, Acquire)`. À partir de cet instant, le BGWorker redirige toutes les nouvelles écritures
longues vers l'arena `1 - W`. L'arena `W` est en lecture seule pour les workers du batch courant.

**Étape 3 — Déduplication et distribution**
Les `entity_id` extraits des slots sont insérés dans le Bit-Vector atomique (ADR
HashSet→Bit-Vector) pour déduplication — un même ID peut apparaître N fois dans le batch
si l'entité a muté N fois dans la fenêtre de tick. La liste dédoublonnée est distribuée aux
threads Rayon/Tokio avec une référence à l'arena `W`. `batch_in_flight` est initialisé.

**Étape 4 — Reprise immédiate**
Le Collector reprend la lecture du ring buffer sans attendre la fin des workers.

**Étape 5 — Libération de l'arena (Tick suivant)**
Au début du Tick T+1, avant toute rotation, le Collector vérifie
`batch_in_flight.load(Acquire) == 0`. Si vrai : `arena_head[W].store(0, Release)` — l'arena
`W` est réinitialisée et disponible pour la prochaine rotation. Si le compteur est encore
non nul, la libération est reportée d'un Tick supplémentaire. Le BGWorker continue d'écrire
dans l'arena `1 - W`, qui dispose de sa propre capacité indépendante.

### 5.2. Résolution du Wrap-Around côté Consommateur

Le slot overflow stocke un seul couple `(offset: u32, length: u32)`. Si `offset + length`
dépasse la capacité de l'arena, la chaîne chevauche physiquement la fin du tampon circulaire.
La résolution est entièrement calculée côté consommateur à la lecture du slot, sans information
supplémentaire stockée par le producteur.

```rust
#[repr(C)]
pub struct ArenaStr {
    pub chunk1_offset: u32,
    pub chunk1_len:    u32,
    pub chunk2_offset: u32, // 0 si la chaîne est contiguë
    pub chunk2_len:    u32, // 0 si la chaîne est contiguë
}

impl ArenaStr {
    #[inline]
    pub fn from_slot(offset: u32, length: u32, arena_capacity: u32) -> Self {
        let end = offset + length;
        if end <= arena_capacity {
            // Cas majoritaire : chaîne contiguë, pas de wrap-around.
            ArenaStr { chunk1_offset: offset, chunk1_len: length,
                       chunk2_offset: 0,      chunk2_len: 0 }
        } else {
            // Wrap-around : la chaîne chevauche la borne supérieure de l'arena.
            let chunk1_len = arena_capacity - offset;
            ArenaStr { chunk1_offset: offset,   chunk1_len,
                       chunk2_offset: 0,         chunk2_len: length - chunk1_len }
        }
    }
}
```

La macro Maud itère nativement sur les deux segments. Le cas wrap-around implique une branche
conditionnelle. Celle-ci est hautement prédictible en conditions normales (quasi-systématiquement
non prise) : le hardware branch predictor l'absorbe sans pénalité de pipeline mesurable.

**Précision formelle :** la propriété garantie est la _haute prédictibilité_ de cette branche,
non son absence. Toute reformulation "sans if/else" dans les documents dérivés est incorrecte.

---

## 6. Backpressure et Resync de Secours

### 6.1. Ring Buffer Plein — Politique Drop

Si `ring_head - ring_tail >= RING_CAPACITY` au moment où le BGWorker tente d'écrire un slot,
l'écriture est abandonnée sans bloquer le processus PostgreSQL :

```rust
// Côté BGWorker (pgrx) — chemin de drop
if ring_head.load(Relaxed) - ring_tail.load(Acquire) >= RING_CAPACITY {
    meta.dropped_events.fetch_add(1, Ordering::Relaxed);
    meta.needs_resync.store(true, Ordering::Release);
    return; // Abandon immédiat, aucun blocage
}
```

**Invariant absolu :** le BGWorker ne bloque jamais sur le ring buffer. Bloquer le BGWorker
bloquerait le backend PostgreSQL appelant, propageant la backpressure jusqu'aux connexions
client — incompatible avec le modèle SPSC et les SLA PostgreSQL.

### 6.2. Resync de Secours

Au début de chaque Tick, avant la lecture du batch, le Collector lit `needs_resync`
(Ordering::Acquire) :

- **Si `true` :** après le traitement du batch normal, le Collector planifie une requête
  de resync complète :

  ```sql
  SELECT id FROM content.core WHERE modified_at >= $last_confirmed_tick_at
  ```

  Les IDs retournés sont injectés dans le Bit-Vector pour déduplication avant dispatch.
  `needs_resync.store(false, Release)`. `last_confirmed_tick_at` (état interne du Collector,
  non stocké dans le SHM) est mis à jour à l'horodatage du Tick courant.

- **Si `false` :** traitement normal, sans requête supplémentaire.

`dropped_events` est exposé dans les métriques opérationnelles (Prometheus ou équivalent).
Une valeur non nulle est un signal que la capacité du ring buffer ou la fréquence de tick max
sont sous-dimensionnées pour la charge observée.

**Relation avec le Bit-Vector (ADR HashSet→Bit-Vector) :** le Bit-Vector de déduplication
reste actif côté Rust même avec le transport SHM. Le ring buffer ne garantit pas l'idempotence
des `entity_id` produits par le BGWorker (une entité mutant N fois dans la fenêtre de tick
génère N slots). La déduplication est systématiquement assurée par le Bit-Vector après lecture
des slots, avant dispatch aux workers — que le batch soit issu du flux normal ou d'un resync.

---

## 7. Récapitulatif des Invariants et Garde-fous

| Invariant                            | Mécanisme                                       | Couche                |
| ------------------------------------ | ----------------------------------------------- | --------------------- |
| Symétrie binaire DDL/Rust            | `static_assertions` à la compilation            | Build-time (DB-Forge) |
| Cohérence de déploiement             | `layout_version` + refus d'attachement          | Boot-time             |
| Attachement sans busy-wait           | `inotify IN_CREATE` sur `/dev/shm/`             | Boot-time (Rust)      |
| Crash recovery SHM                   | `shm_unlink` + `O_EXCL` au redémarrage BGWorker | Boot-time (PG)        |
| Non-partage de ligne de cache (SPSC) | Slots 64B = une ligne de cache L1               | Layout physique       |
| Non-pollution cache L1/L2 (TOAST)    | NT stores + `_mm_sfence()` / `dmb ish`          | Write path BGWorker   |
| Ordre NT store → slot visible        | Barrière mémoire avant écriture slot            | Write path BGWorker   |
| Isolation arenas lecture/écriture    | Ping-Pong sur `arena_write_index`               | Lock-free rotation    |
| Wrap-around arena                    | `ArenaStr::from_slot` côté consommateur         | Read path Rust        |
| Absence de blocage producteur        | Drop atomique + `needs_resync`                  | Backpressure          |
| Cohérence après drop                 | Resync par `modified_at` + Bit-Vector           | Tick suivant          |
| Déduplication IDs                    | Bit-Vector atomique (ADR HashSet→Bit-Vector)    | Collector Rust        |
