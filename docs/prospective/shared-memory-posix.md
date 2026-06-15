# Document d'Architecture Système : Couplage `POSIX shm` (Zéro-Copie)

**Projet :** Marius Phase 2  
**Objet :** Transition de la Zéro-Indirection (`pgwire`) vers la Zéro-Copie (`mmap`) via mémoire partagée.  
**Statut :** Spécification technique arrêtée.  
**Révision :** Intègre les arbitrages d'audit — slots 64B, wrap-around consommateur, attachement
`inotify`, backpressure drop/resync, Ping-Pong arena, NT stores, BatchGuard RAII, snapshot MVCC,
LSN high-water mark, contrainte de capacité d'arena, politique de versioning ABI.

---

> Cette spécification est compète mais est réservée pour une version 2 de Marius. Il s'agit ici d'imaginer une "soudure à froid" (Cold Welding) entre PostgreSQL et Rust.

---

## 1. Topologie des Processus et Invariant SPSC

La communication s'établit via un segment de mémoire partagée POSIX (`/dev/shm/marius_engine`),
structuré comme un bus système **SPSC (Single-Producer Single-Consumer) lock-free**.

- **Producteur Unique (PostgreSQL BGWorker) :** L'extension `pgrx` déploie un Background Worker
  dédié. Les backends PostgreSQL (`INSERT/UPDATE`) signalent le BGWorker sans écrire directement
  dans le SHM. Le BGWorker est le seul processus autorisé à écrire dans le segment — ce qui
  élimine le _cache bouncing_ des instructions `compare_exchange` inhérent à un modèle MPSC.
  L'unicité du BGWorker est garantie structurellement par l'API PostgreSQL : `RegisterBackgroundWorker`
  avec une seule inscription dans le code d'initialisation de la bibliothèque partagée garantit
  au niveau de l'OS (fork) qu'un et un seul processus tournera. Aucun verrou applicatif
  (`pg_advisory_lock` ou autre) n'est nécessaire ni permis.

- **Consommateur Unique (Marius Core / Collector) :** Le thread principal de l'Orchestrator Rust
  dépile les événements du ring buffer en O(1) et gère les pointeurs de libération de l'arena
  active.

- **Séquence de Boot (Ownership) :** Le BGWorker est le propriétaire matériel du segment
  (`O_CREAT | O_RDWR | O_EXCL`) et l'initialise entièrement avant de le signaler. Le processus
  Rust s'y attache via `inotify` (§3).

---

## 2. Layout Binaire et Adressage Relatif

L'adressage interne au segment proscrit les pointeurs absolus pour neutraliser l'ASLR.
Toute adresse est calculée dynamiquement : `base_ptr.add(offset)`.

Le segment contient quatre zones physiquement adjacentes.

### 2.1. Zone Meta (En-tête de validation et synchronisation)

Structure `#[repr(C)]` portant les compteurs atomiques du ring buffer, les primitives
de synchronisation des arenas, et le high-water mark LSN :

```rust
#[repr(C)]
pub struct ShmMeta {
    pub layout_version:       u64,           // Hash DDL compilé : env!("DDL_HASH")
    pub ring_head:            AtomicU64,     // Prochain slot à écrire (producteur)
    pub ring_tail:            AtomicU64,     // Prochain slot à lire  (consommateur)
    pub arena_write_index:    AtomicU8,      // Arena active en écriture : 0 ou 1
    pub arena_head:           [AtomicU32; 2],// Curseur d'écriture pour chaque arena
    pub last_processed_lsn:   AtomicU64,    // High-water mark WAL du BGWorker (XLogRecPtr)
    pub dropped_events:       AtomicU64,    // Compteur d'événements perdus (backpressure)
    pub needs_resync:         AtomicBool,   // Drapeau : au moins un drop s'est produit
    _pad: [u8; ...],                        // Padding jusqu'à 128 octets (alignement 64B × 2)
}
```

**`layout_version`** : hash statique du DDL injecté à la compilation par la DB-Forge
(`env!("DDL_HASH")`). Si le Collector Rust détecte une asymétrie à l'attachement, il refuse le
`mmap`, logue une erreur fatale et se termine.

**`last_processed_lsn`** : dernier `XLogRecPtr` (u64) traité avec succès par le BGWorker.
Mis à jour par le BGWorker après chaque écriture dans le ring buffer. Non mis à jour lors
d'un drop — préservant ainsi le dernier point de cohérence connu pour le resync (§6.2).

**Relation avec les `static_assertions` (risques.md §1) :** ces deux gardes sont complémentaires
et non substituables. Les `static_assertions` bloquent la _compilation_ en cas de désalignement
DDL/Rust. Le `layout_version` bloque le _boot_ en cas de déploiement partiel. Les deux
mécanismes doivent coexister.

### 2.2. Zone Primaire — Ring Buffer (Slots de 64 octets)

Chaque slot occupe exactement **64 octets**, soit une ligne de cache L1 complète.

**Invariant anti-false-sharing :** dans un SPSC, le producteur écrit en `ring_head` pendant
que le consommateur lit en `ring_tail`. Avec des slots de 32 octets, deux slots adjacents
partagent une ligne de cache — le consommateur charge en L1 le slot qu'il lit _et_ le slot
suivant en cours d'écriture par le producteur. L'invalidation de cohérence de cache résultante
génère une contention inutile. Des slots de 64 octets y mettent fin structurellement.

**Layout SSO du slot (64 octets) :**

```
Offset  0 : tag : u8
  — tag == 0 (Inline) : octets 1..63 → payload inline (chaîne ≤ 63 octets)
  — tag == 1 (Arena)  : octets 1..4  → offset: u32 dans l'arena active
                        octets 5..8  → length: u32
                        octets 9..63 → padding mort
```

- **Inline (tag == 0) :** chaîne ≤ 63 octets copiée directement dans le slot. Zéro indirection.
- **Arena (tag == 1) :** `offset` et `length` désignent le bloc de données dans la bump arena
  active. Le wrap-around éventuel est résolu côté consommateur (§5.2).

### 2.3. Zone Secondaire — Bump Arenas en Ping-Pong

Deux arenas circulaires de capacité égale (`arena[0]` et `arena[1]`), physiquement contiguës
dans le segment. `arena_write_index` désigne l'arena dans laquelle le BGWorker écrit.

**Contrainte de capacité :** avant toute écriture dans l'arena, le BGWorker vérifie que la
taille post-détoastage du bloc est inférieure à `arena_capacity / 2`. Si cette borne est
dépassée, l'écriture est abandonnée (politique drop, §6.1). Cette borne conservatrice garantit
qu'aucun item seul ne monopolise plus d'une demi-arena, préservant la capacité de rotation
pour les items suivants indépendamment de la position courante du curseur. La DB-Forge impose
des contraintes DDL (`VARCHAR(N)`) sur toutes les colonnes TEXT pour qu'un tuple légal ne puisse
structurellement jamais atteindre cette limite en conditions normales.

**Objectif du Ping-Pong :** découpler le cycle de vie des workers Rust — qui lisent depuis l'arena
`W` figée au moment du Tick T — du cycle de production BGWorker — qui continue d'écrire dans
l'arena `1 - W` pendant le rendu. La rotation est gouvernée par le Collector au `flush` (§5.1).

---

## 3. Boot et Attachement via `inotify`

**Contrainte :** un busy-wait sur `/dev/shm/` est inacceptable. Un sleep arbitraire est
non-déterministe.

**Protocole d'attachement :**

1. Le processus Rust enregistre un watcher `inotify` sur `/dev/shm/`, filtrant `IN_CREATE`.
2. Le BGWorker crée le segment (`O_CREAT | O_RDWR | O_EXCL`), le redimensionne (`ftruncate`),
   initialise la Zone Meta entièrement (y compris `last_processed_lsn = 0`), puis signale
   la disponibilité par la création du fichier elle-même.
3. À la réception de `IN_CREATE` pour `marius_engine`, le processus Rust ouvre le descripteur,
   appelle `mmap(PROT_READ | PROT_WRITE, MAP_SHARED)`, lit `meta.layout_version`.
4. Si le hash correspond : attachement validé, le Collector démarre.
5. Si le hash diverge : fermeture, log fatal `[BOOT] DDL hash mismatch: expected {A}, found {B}`,
   terminaison. Le redéploiement synchronisé des deux artefacts (extension PG + binaire Rust)
   est requis.

**Crash recovery :** `O_EXCL` garantit qu'aucun segment existant n'est réutilisé. Si le fichier
existe déjà au démarrage du BGWorker (crash précédent), `shm_unlink` est appelé avant
recréation. Un segment aux pointeurs Head/Tail potentiellement corrompus n'est jamais hérité.

---

## 4. Traitement TOAST et Stores Non-Temporels (NT Stores)

### 4.1. Détoastage, Isolation MVCC et Linéarisation

Le BGWorker appelle `pg_detoast_datum` pour rapatrier les chunks TOAST dans son espace
d'adressage local. Pour garantir que la version du datum lue correspond exactement à la version
du tuple qui a déclenché l'événement — et non à une version modifiée par un `UPDATE` concurrent
ou purgée par `VACUUM` pendant le transfert — le détoastage doit s'exécuter au sein d'un snapshot
transactionnel explicite :

```c
/* Séquence obligatoire côté BGWorker (API C PostgreSQL) */
StartTransactionCommand();
PushActiveSnapshot(GetTransactionSnapshot()); /* Fige la vision MVCC */

/* Appel sécurisé : VACUUM ne peut pas réclamer les blocs TOAST
   référencés par ce snapshot pendant toute la durée du bloc. */
detoasted = pg_detoast_datum(datum_ptr);

/* Copie vers la bump arena (NT stores, §4.2) */
nt_copy(arena_dst, VARDATA(detoasted), VARSIZE_ANY_EXHDR(detoasted));

PopActiveSnapshot();
CommitTransactionCommand();
```

Pour éviter les fuites mémoire (OOM) liées au cycle de vie persistant du BGWorker,
chaque itération s'exécute dans un `MemoryContext` éphémère (`AllocSetContextCreate`),
réinitialisé (`MemoryContextReset`) immédiatement après le transfert vers l'arena.

### 4.2. Instructions NT (_Non-Temporal Stores_)

La propriété de non-pollution du cache L1/L2 lors de la copie vers la bump arena exige
l'usage explicite d'instructions NT. Sans elles, toute écriture transite par le cache du cœur
producteur et invalide les lignes correspondantes sur les cœurs consommateurs.

**Implémentation (x86-64, SSE2) :**

```rust
/// Copie non-temporelle vers la bump arena.
/// Préconditions : `dst` aligné sur 16 octets ; `len` multiple de 16 pour le bloc principal.
/// Appeler `_mm_sfence()` après cette fonction, avant d'écrire le slot.
unsafe fn nt_copy(dst: *mut u8, src: *const u8, len: usize) {
    use std::arch::x86_64::*;
    let mut i = 0;
    while i + 16 <= len {
        let chunk = _mm_loadu_si128(src.add(i) as *const __m128i);
        _mm_stream_si128(dst.add(i) as *mut __m128i, chunk);
        i += 16;
    }
    // Reliquat (len non multiple de 16) par copie scalaire normale.
    while i < len {
        dst.add(i).write(src.add(i).read());
        i += 1;
    }
}
```

Sur AArch64 : `stnp` (Non-Temporal Store Pair) ou `STNT1` (SVE), suivi d'un `dmb ish`.

**Barrière obligatoire :** après `nt_copy` et avant l'écriture du slot dans le ring buffer,
`_mm_sfence()` (x86) / `dmb ish` (ARM) garantit l'ordre global d'écriture. Sans cette barrière,
le consommateur peut lire l'offset dans le slot avant que la copie NT ne soit visible en mémoire
principale.

**Périmètre :** NT stores exclusivement pour les copies vers la bump arena. Les écritures dans
le ring buffer (slots 64 octets, séquentielles) transitent par le cache normal — elles doivent
être visibles en L1 pour le consommateur au prochain Tick.

---

## 5. Modèle de Synchronisation Lock-Free

### 5.1. Adaptive Tick, Rotation Ping-Pong et BatchGuard

La séquence suivante s'exécute intégralement sur le thread du Collector à chaque `flush` :

**Étape 1 — Lecture du batch**
Le Collector lit un lot de slots depuis `ring_tail` jusqu'à `ring_head`
(Ordering::Acquire sur `ring_head`). Pour chaque slot `tag == 1`, il note l'index d'arena
courant `W = arena_write_index.load(Acquire)`.

**Étape 2 — Rotation de l'arena**
CAS sur `arena_write_index` : `W → (1 - W)`. Le BGWorker redirige toutes les nouvelles
écritures longues vers l'arena `1 - W`. L'arena `W` est en lecture seule pour les workers
du batch courant.

**Étape 3 — Déduplication et distribution avec BatchGuard**
Les `entity_id` extraits sont insérés dans le Bit-Vector atomique pour déduplication
(ADR HashSet→Bit-Vector — actif indépendamment du transport SHM, car le BGWorker peut écrire
le même `entity_id` N fois dans la fenêtre de tick). La liste dédoublonnée est distribuée
aux threads Rayon/Tokio. Chaque worker reçoit un `BatchGuard` :

```rust
/// RAII guard : garantit la décrémentation de batch_in_flight même en cas de panique.
struct BatchGuard<'a> {
    counter: &'a AtomicI32,
}

impl<'a> Drop for BatchGuard<'a> {
    fn drop(&mut self) {
        // Appelé par le stack unwinding Rust en cas de panique du worker.
        // Zéro overhead en cas nominal ; sécurité totale en cas de panique.
        self.counter.fetch_sub(1, Ordering::Release);
    }
}
```

L'usage de `catch_unwind` est proscrit : il introduit une barrière d'exécution logicielle
incompatible avec le pipeline DOD. Le RAII via `Drop` délègue la garantie au compilateur Rust,
sans coût à l'exécution sur le chemin nominal.

**Étape 4 — Reprise immédiate**
Le Collector reprend la lecture du ring buffer sans attendre la fin des workers.

**Étape 5 — Libération de l'arena**
Au début du Tick suivant, avant toute rotation : si `batch_in_flight.load(Acquire) == 0`,
alors `arena_head[W].store(0, Release)` — l'arena `W` est réinitialisée et disponible. Si le
compteur est encore non nul (workers du Tick T encore en cours), la libération est reportée
d'un Tick. Le BGWorker continue d'écrire dans `1 - W`, qui dispose de sa capacité indépendante.

### 5.2. Résolution du Wrap-Around côté Consommateur

Le slot overflow stocke un seul couple `(offset: u32, length: u32)`. La résolution est calculée
côté consommateur à la lecture, sans information supplémentaire dans le slot :

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
            ArenaStr { chunk1_offset: offset, chunk1_len: length,
                       chunk2_offset: 0,      chunk2_len: 0 }
        } else {
            let chunk1_len = arena_capacity - offset;
            ArenaStr { chunk1_offset: offset, chunk1_len,
                       chunk2_offset: 0,      chunk2_len: length - chunk1_len }
        }
    }
}
```

Le pipeline de rendu (AOT / `push_str`) itère nativement sur les deux segments. La branche conditionnelle est hautement
prédictible (quasi-systématiquement non prise). **Précision formelle :** la propriété garantie
est la _haute prédictibilité_ de cette branche, non son absence. Toute reformulation "sans
if/else" dans les documents dérivés est incorrecte.

---

## 6. Backpressure et Resync de Secours

### 6.1. Ring Buffer Plein ou Arena Saturée — Politique Drop

Le BGWorker abandonne l'écriture sans bloquer dans deux cas :

1. **Ring buffer plein** : `ring_head - ring_tail >= RING_CAPACITY`.
2. **Taille post-détoastage** : `detoasted_size > arena_capacity / 2`.

Dans les deux cas :

```rust
// Côté BGWorker (pgrx)
meta.dropped_events.fetch_add(1, Ordering::Relaxed);
meta.needs_resync.store(true, Ordering::Release);
// last_processed_lsn N'EST PAS mis à jour — préserve le dernier point de cohérence.
return;
```

**Invariant absolu :** le BGWorker ne bloque jamais. Bloquer le BGWorker bloquerait le backend
PostgreSQL appelant, propageant la backpressure jusqu'aux connexions client.

### 6.2. Resync de Secours via LSN

**Pourquoi pas `modified_at` :** un `TIMESTAMPTZ` n'est pas un compteur monotone strict.
Les transactions concurrentes, la dérive NTP et la résolution à la microseconde rendent possible
la perte silencieuse de mutations si plusieurs commits tombent dans la même fenêtre temporelle.

**Mécanisme LSN :**

Le LSN (Log Sequence Number) est un pointeur binaire absolu (`XLogRecPtr`, u64) représentant
l'offset exact dans le WAL PostgreSQL. Il est strictement monotone et exempt de toute dérive.

**Prérequis DDL :** la DB-Forge ajoute une colonne `walsn pg_lsn DEFAULT '0/0'` aux tables
de contenu. Les fonctions SECURITY DEFINER du write path (seule interface d'écriture autorisée)
peuplent cette colonne systématiquement :

```sql
-- Dans chaque fonction SECURITY DEFINER de mutation
NEW.walsn := pg_current_wal_lsn();
```

**Mise à jour du high-water mark :** après chaque écriture réussie dans le ring buffer, le
BGWorker met à jour `meta.last_processed_lsn` avec le `XLogRecPtr` courant
(obtenu via `GetXLogWriteRecPtr()` depuis l'API C pgrx). Sur un drop, cette valeur n'est pas
modifiée — elle reste le dernier LSN de cohérence garanti.

**Requête de resync :** au début de chaque Tick, le Collector lit `needs_resync`
(Ordering::Acquire) :

- **Si `true` :** après le batch normal, le Collector exécute :

  ```sql
  SELECT id FROM content.core
  WHERE walsn > $1::pg_lsn
  ORDER BY walsn ASC
  ```

  où `$1` est `last_processed_lsn` lu depuis la Meta zone, converti en représentation texte
  `pg_lsn` (`'A/BBBBBBBB'`). Les IDs retournés sont injectés dans le Bit-Vector pour dispatch.
  `needs_resync.store(false, Release)`. `last_processed_lsn` est mis à jour au LSN courant.

- **Si `false` :** traitement normal.

`dropped_events` est exposé dans les métriques opérationnelles. Une valeur non nulle indique
que le ring buffer ou le tick max sont sous-dimensionnés pour la charge observée.

---

## 7. Politique de Versioning et Rupture d'ABI C

Le moteur Marius assume un couplage physique fort à une version majeure spécifique de PostgreSQL
(actuellement **v17 LTS**) via l'extension `pgrx`. L'exploitation de l'API C interne
(`pg_detoast_datum`, `MemoryContext`, snapshots MVCC, `GetXLogWriteRecPtr`) expose le BGWorker
aux ruptures d'ABI inhérentes aux sauts de versions majeures de PostgreSQL. La version de `pgrx`
est elle-même couplée à la version majeure PG et constitue une dépendance de compilation
à versionner conjointement au BGWorker.

En réponse, l'architecture pose les invariants suivants :

1. **Adhérence LTS stricte :** Le cycle de vie du moteur s'aligne exclusivement sur les versions
   stables de PostgreSQL (support de 5 ans). La validation continue sur des versions de
   développement (bêta) est proscrite pour éviter le gaspillage de cycles de R&D sur des ABI
   volatiles.

2. **Fail-Fast Déterministe :** Un saut de version majeure n'entraînera jamais de corruption
   silencieuse. Toute modification des tailles de structures, des offsets d'alignement ou des
   signatures de fonctions C internes fera échouer les `static_assertions` (Build-Time) ou
   provoquera le rejet de l'attachement via le hash `layout_version` (Boot-Time).

3. **Contrat de Mise à Jour :** Le passage à une nouvelle version majeure (ex: v17 → v18) est
   classifié comme une **migration d'infrastructure**, non comme une mise à jour logicielle. Il
   exige : l'audit du code C généré (pointeurs TOAST, MemoryContext API), la mise à jour de la
   version `pgrx` dans `Cargo.toml`, et la recompilation explicite et synchronisée du BGWorker
   et du Core Rust. Le hash `layout_version` est régénéré lors de cette compilation, forçant
   une validation de bout en bout au prochain boot.

---

## 8. Récapitulatif des Invariants et Garde-fous

| Invariant                         | Mécanisme                                             | Couche                |
| --------------------------------- | ----------------------------------------------------- | --------------------- |
| Symétrie binaire DDL/Rust         | `static_assertions` à la compilation                  | Build-time (DB-Forge) |
| Cohérence de déploiement          | `layout_version` + refus d'attachement                | Boot-time             |
| Attachement sans busy-wait        | `inotify IN_CREATE` sur `/dev/shm/`                   | Boot-time (Rust)      |
| Crash recovery SHM                | `shm_unlink` + `O_EXCL` au redémarrage BGWorker       | Boot-time (PG)        |
| Unicité du producteur             | `RegisterBackgroundWorker` (fork OS)                  | Architecture pgrx     |
| Non-partage de ligne de cache     | Slots 64B = une ligne de cache L1                     | Layout physique       |
| Non-pollution cache L1/L2 (TOAST) | NT stores + `_mm_sfence()` / `dmb ish`                | Write path BGWorker   |
| Ordre NT store → slot visible     | Barrière mémoire avant écriture slot                  | Write path BGWorker   |
| Isolation MVCC du détoastage      | `PushActiveSnapshot` / `CommitTransactionCommand`     | Write path BGWorker   |
| Isolation arenas lecture/écriture | Ping-Pong sur `arena_write_index`                     | Lock-free rotation    |
| Wrap-around arena                 | `ArenaStr::from_slot` côté consommateur               | Read path Rust        |
| Robustesse panique worker         | `BatchGuard` RAII (Drop)                              | Workers Rayon/Tokio   |
| Absence de blocage producteur     | Drop atomique + `needs_resync`                        | Backpressure          |
| Contrainte taille item arena      | Drop si `detoasted_size > arena_capacity / 2`         | Write path BGWorker   |
| Cohérence après drop              | Resync par LSN (`walsn > last_processed_lsn`)         | Tick suivant          |
| Déduplication IDs                 | Bit-Vector atomique (ADR HashSet→Bit-Vector)          | Collector Rust        |
| Stabilité ABI PostgreSQL          | Adhérence LTS + audit explicite sur migration majeure | Gouvernance           |

---

Document créé le 21 mai 2026.
Révisé le 15 juin 2026.
