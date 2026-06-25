# Spécification Technique & Roadmap — Fonction de Fusion RCU
**Projet Marius — Moteur AOT / RCU Lock-Free en Rust**
Statut : architecture verrouillée, sans point ouvert restant. Prête pour implémentation (Phase 4.1).

---

# Partie 1 — Spécification de la fonction de fusion

## 1.1 Invariants systémiques et physiques

### Invariants de structure (validés, non négociables)

| ID | Invariant |
|---|---|
| I1 | Catalogue shardé en buckets d'environ 50 Mo. Routage sans indirection mémoire : `bucket_id = hash(entity_id)`. **Confirmé** : ce seuil est une limite souple globale, incluant payload **et** index. L'index ne pesant que quelques mégaoctets en pratique, il est absorbé dans ce budget sans impact significatif sur la borne pratique (voir I7). |
| I2 | `DeltaBatch` en AoS dense, `DeltaEntry` `#[repr(C)]` de 16 octets (`i64` + `u32` + `u32`). Choix DOD justifié : `entity_id` (comparaison) et `offset`/`length` (copie) sont consommés ensemble à chaque itération — la colocalisation AoS maximise le pré-fetching matériel sur ce pattern d'accès couplé. |
| I3 | Format Packfile : blobs HTML contigus, suivis de l'index en queue de fichier. `PackfileEntry` = structure identique à `DeltaEntry`. |

### Contrats algorithmiques (stricts)

| ID | Contrat |
|---|---|
| C1 | `DeltaBatch.entries` strictement trié par `entity_id`. |
| C2 | Index de l'ancien packfile strictement trié par `entity_id`. |
| C3 | Merge en balayage linéaire `O(N + D)` (*two-pointer sweep*). Aucune allocation dans la boucle interne (pas de `HashMap`, `HashSet`, ni `binary_search`). |

### Invariant nommé — Contrat DELETE

> **Aucun fragment vivant ne peut avoir `length == 0`.**

`length == 0` joue aujourd'hui un double rôle : donnée physique (taille du blob) et marqueur logique (suppression). Nommer explicitement cette règle lève l'ambiguïté pour toute évolution future introduisant un fragment vide légitime (placeholder, redirection symbolique) sans changer l'encodage binaire actuel. C'est cet invariant qui justifie l'hypothèse de la table de décision (1.2) selon laquelle une entrée *survivante* dans `old_index` a toujours `length > 0`.

### Invariants dérivés (auto-entretenus)

**Propriété 1 — ordre physique du payload** : l'ordre physique des blobs dans le fichier `.bin`/`.tmp` est identique à l'ordre logique de l'index (`entity_id` croissant), à tout instant du cycle de vie du système.

**Preuve par récurrence** :
- *Cas de base* : le premier packfile est généré par `SELECT ... ORDER BY id` suivi d'une écriture séquentielle → ordre physique = ordre logique.
- *Hérédité* : `merge_sweep` consomme deux flux triés (C1, C2) et produit, par construction d'un merge de listes triées, un flux de sortie strictement croissant en `entity_id`, écrit séquentiellement. Si l'invariant est vrai en entrée, il reste vrai en sortie.

**Conséquence exploitée** : les plages d'entrées consécutives non modifiées (*runs*) sont garanties contiguës en mémoire source et destination → autorise le `memcpy` par bloc plutôt qu'entrée par entrée (cf. 1.2).

**Fragilité à documenter dans le code** : cette propriété est un *bootstrap* — garantie par l'algorithme lui-même, pas par une contrainte externe au système de fichiers. Toute évolution future introduisant une écriture non séquentielle dans `out_blob` (ex. parallélisation par sous-plages) invalide silencieusement l'invariant pour le cycle suivant. À garder en commentaire au-dessus de `merge_sweep`.

**Propriété 2 — continuité du payload (partition sans trou ni recouvrement)** :

> Pour toute paire d'entrées consécutives `i, i+1` de l'index : `offset[i+1] == offset[i] + length[i]`.

**Preuve** : même schéma de récurrence que la propriété 1. Cas de base — l'écriture initiale séquentielle ne laisse aucun trou. Hérédité — `merge_sweep` avance son curseur d'écriture dans `out_blob` exactement de `length` à chaque entrée écrite (run, INSERT ou UPDATE), jamais davantage, jamais moins. Aucune opération du sweep ne peut introduire un trou ou un recouvrement.

**Intérêt pratique** : cette propriété permet de construire un validateur offline de bucket qui ne nécessite aucune connaissance du reste du système — il suffit de parcourir l'index et de vérifier la relation de continuité, indépendamment de toute autre fusion passée ou future.

### I7 — Borne sur le nombre d'entrées par bucket (confirmée)

Deux bornes distinctes, désormais calculables sous le périmètre confirmé (50 Mo = payload + index) :

- **Borne dure (garantie par construction)** : chaque entrée vivante occupe au moins 16 octets dans l'index. Si l'index consommait la totalité du budget, `N ≤ 50 Mo / 16 octets ≈ 3,27 millions` d'entrées. Plafond théorique uniquement — en pratique l'index ne pèse que quelques mégaoctets (confirmé), donc ce cas dégénéré (payloads quasi nuls) ne se présente pas en conditions réelles.
- **Borne pratique (mesurée)** : avec une taille moyenne de fragment HTML réaliste (~100-200 octets) et un index de quelques Mo, `N` typique se situe autour de 200k-400k entrées par bucket. C'est cette valeur qui sert de référence pour le budget de cycle (déterminisme "soft real-time").

**Coût du merge** : `O(payload_shard) + O(entry_count_shard)`. Sous le périmètre confirmé, le second terme reste marginal face au premier en conditions réelles — mais conserve la formulation à deux termes dans le code et les commentaires, pour rester correct si la distribution des tailles de fragments évoluait.

---

## 1.2 Table de décision algorithmique (two-pointer sweep + runs)

**Curseurs** : `i` sur `old_index`, `j` sur `delta.entries`. Aucun curseur sur le payload — sa position se déduit de `offset`/`length`.

**Principe** : au lieu de traiter chaque entrée non modifiée individuellement, le sweep détecte les *runs* (séquences maximales d'entrées consécutives en `old < delta`) et les copie en un seul bloc, pour le payload **et** pour l'index.

| État / Condition | Action | Avancement |
|---|---|---|
| `old[i].id < delta[j].id` | Étendre la run courante (pas de copie immédiate) | `i += 1`, répéter tant que la condition tient |
| Fin de run (`old[i].id ≥ delta[j].id`, ou `i` épuisé) | **Flush de la run** : un seul `memcpy` sur la plage payload `[run_start..i)`, un seul `extend_from_slice` sur `old_index[run_start..i)` | — |
| `old[i].id > delta[j].id` et `delta[j].length == 0` | DELETE sur une entité absente (no-op, anomalie inoffensive sous hypothèse de delta cohérent) | `j += 1` |
| `old[i].id > delta[j].id` et `delta[j].length > 0` | INSERT : copier depuis `delta.payload`, écrire nouvelle entrée | `j += 1` |
| `old[i].id == delta[j].id` et `delta[j].length == 0` | DELETE : ne rien copier, ne rien écrire dans l'index | `i += 1`, `j += 1` |
| `old[i].id == delta[j].id` et `delta[j].length > 0` | UPDATE : copier depuis `delta.payload` (remplace), écrire nouvelle entrée | `i += 1`, `j += 1` |
| Drainage (un flux épuisé) | Continuer avec l'autre flux traité comme infini jusqu'à épuisement ; flush de la run en cours si applicable | — |

**Point de structure** : le `match` sur l'`Ordering` des `entity_id` ne comporte qu'un seul test imbriqué (`length == 0`), localisé dans la branche `Equal` et dans la branche `Greater` côté delta. Pas de duplication de la logique DELETE ailleurs dans la boucle.

**Hypothèse à confirmer** : une entrée *survivante* dans `old_index` a toujours `length > 0` (un DELETE déjà appliqué lors d'une fusion précédente ne réapparaît pas dans l'index). Si cette hypothèse est fausse, un check supplémentaire côté `old` serait nécessaire.

---

## 1.3 Structures de données et signature (DOD, zéro-allocation hot path)

```rust
#[repr(C)]
#[derive(Copy, Clone)]
pub struct PackfileEntry {
    pub entity_id: i64,
    pub offset: u32,
    pub length: u32,
}

pub struct MergeReport {
    pub bytes_written: u64,
    pub entries_written: u32,
    pub deletes_applied: u32,
    pub runs_count: u32,            // nombre de runs détectées (payload + index)
    pub bytes_copied_from_old: u64, // volume issu de runs inchangées (old_blob)
    pub bytes_inserted_from_delta: u64, // volume issu d'INSERT/UPDATE (delta.payload)
}

/// Balayage two-pointer avec détection de runs, zéro-allocation dans la boucle interne.
///
/// `old_blob` / `out_blob` : mmap. `out_blob` est dimensionné en BORNE SUPÉRIEURE
/// (old_blob.len() + delta.payload.len()) via un `ftruncate` haut préalable côté appelant.
/// L'appelant effectue le `ftruncate` bas final avec `MergeReport.bytes_written`,
/// AVANT le `rename` atomique (bascule ArcSwap déjà codée).
///
/// `out_index` : seul point d'allocation, réservé en amont via
/// `Vec::with_capacity(old_index.len() + delta.entries.len())`, donc hors du
/// chemin chaud mesuré.
///
/// Invariant exploité : `old_blob` et `old_index` sont physiquement ordonnés
/// par `entity_id` (propriété auto-entretenue, voir section 1.1). Toute
/// modification future cassant l'ordre d'écriture séquentiel invalide cet
/// invariant pour le cycle suivant — à vérifier avant toute parallélisation
/// de cette fonction.
pub fn merge_sweep(
    old_blob: &[u8],
    old_index: &[PackfileEntry],
    delta: &DeltaBatch,
    out_blob: &mut [u8],
    out_index: &mut Vec<PackfileEntry>,
) -> MergeReport;
```

**Conformité DOD** :
- Pas de `Rc`/`Arc`/`Box` dans la signature : uniquement slices et références brutes.
- `out_blob: &mut [u8]` plutôt qu'un `Write` générique : pas de vtable, écriture directe par slice, cohérent avec le `mmap` en lecture déjà utilisé sur l'ancien packfile.
- `out_index` alloué une seule fois, avant l'appel.

**Usage de `MergeReport` au-delà de l'exécution courante** : `bytes_copied_from_old` et `bytes_inserted_from_delta` ne servent pas l'algorithme lui-même — ils transforment le rapport en artefact de profilage structurel, exploitable plusieurs mois après mise en production pour répondre sans instrumentation supplémentaire à : la part des merges qui ne sont que des copies de runs, la densité réelle des deltas observée en production, et si le découpage en buckets (I1) reste adapté au comportement observé.

---

# Partie 2 — Roadmap d'implémentation

## Phase 4.1 — Moteur Algorithmique (CPU-Bound)

**Périmètre** : `merge_sweep` en pur calcul, zéro I/O. Toutes les entrées/sorties sont des slices en mémoire (simulation du `mmap` via `Vec<u8>` en test).

**Tâches** :
- Implémenter le two-pointer sweep selon la table de décision 1.2.
- Implémenter la détection de runs sur le payload **et** sur l'index (`extend_from_slice` pour l'index, `memcpy`/`copy_from_slice` pour le payload).
- Tests unitaires couvrant : DELETE, INSERT, UPDATE, runs longues, drainage flux gauche, drainage flux droit, bucket vide, delta vide, recouvrement total (delta == old en entier).

**Critère de sortie** :
- Couverture à 100 % des branches de la table de décision.
- Zéro allocation mesurée dans la boucle (vérification via allocateur de test instrumenté, ou inspection de l'assembleur généré pour la boucle interne).

## Phase 4.2 — Plomberie Système (I/O-Bound)

**Périmètre** : intégration de `merge_sweep` dans le cycle de vie réel du fichier `.tmp`.

**Tâches** :
- `ftruncate` haut : dimensionner `.tmp` à `old_blob.len() + delta.payload.len()` (borne supérieure).
- `mmap` en écriture du `.tmp` (crate `memmap2` ou équivalent).
- Appel à `merge_sweep` sur les slices mappées.
- `ftruncate` bas : réduire à la taille réelle (`MergeReport.bytes_written`) — opération de métadonnées `O(1)`, pas de recopie.
- **Persistance durable avant bascule** : `msync(MS_SYNC)` sur la région mappée (ou `fsync(fd)` sur le descripteur du `.tmp`) **avant** le `rename`. Sans cette étape, le `rename` peut publier un fichier dont le contenu réside encore uniquement en page cache : un crash entre le `rename` et le *writeback* du noyau romprait la garantie implicite de toute la bascule `ArcSwap`. Propriété formelle visée : *un `rename` n'est effectué que sur un fichier déjà durablement persisté.*
  - Remarque additionnelle : selon le système de fichiers cible, la durabilité du `rename` lui-même (pas seulement du contenu) peut nécessiter un `fsync` du répertoire parent (sémantique POSIX) — à vérifier avant de considérer ce point clos.
- `rename` atomique → intégration avec la bascule `ArcSwap` déjà codée.

**Critère de sortie** :
- Test d'intégration : fichier `.tmp` final bit-à-bit identique à un merge de référence calculé en pur `Vec<u8>` (Phase 4.1).
- Test de robustesse : interruption simulée avant le `rename` ne doit jamais corrompre l'ancien packfile (le `.tmp` reste orphelin, l'ancien fichier n'est jamais touché tant que le `rename` n'a pas eu lieu).
- Test d'ordonnancement : vérifier que le `fsync`/`msync` est systématiquement appelé et complété avant l'appel à `rename` (ordre observable, ex. via instrumentation ou `strace` en test).

## Phase 4.3 — Orchestration & Régulation (Async/Concurrency)

**Périmètre** : limiter le nombre d'écritures `mmap` concurrentes pour éviter un *dirty-page storm* (accumulation de pages modifiées en attente d'écriture disque, provoquant un pic de latence noyau lors du *writeback*).

**Tâches** :
- Sémaphore (`tokio::sync::Semaphore`) borné dans l'orchestrateur, acquis avant le `ftruncate` haut, libéré après le `rename`.
- **Confirmé** : `mmap`, `ftruncate` et `rename` sont des appels système bloquants. Le Dispatcher étant déjà massivement asynchrone (Tokio/SQLx pour l'orchestration, Axum pour l'API de lecture), ces appels doivent être encapsulés dans `tokio::task::spawn_blocking` pour ne pas affamer les workers de l'executor — sans cette précaution, ils bloqueraient un thread *worker* pendant toute la durée du sweep (potentiellement quelques millisecondes pour un bucket de 50 Mo). Le sémaphore (`tokio::sync::Semaphore`) régule l'accès à ce bloc `ftruncate`/`mmap`/`merge_sweep`/`ftruncate`/`rename` exécuté via `spawn_blocking`.
- Intégration avec le Dispatcher existant (point d'entrée, gestion d'erreur en cas d'échec d'acquisition ou de timeout).
- La limite du sémaphore (nombre de merges simultanés) est une propriété de la machine cible, pas une constante architecturale — l'exposer en paramètre de configuration runtime, avec une métrique observable (nombre de permits en attente/occupés), plutôt qu'une valeur figée dans le code.

**Critère de sortie** :
- Test de charge : N buckets traités en concurrence contrôlée, mesure de la latence `fsync` sous charge maximale, validation que le sémaphore borne effectivement le pic de pages dirty (observable via `/proc/vmstat` ou équivalent).

---

## Annexe — Décisions actées (clôture des points ouverts)

Les deux points laissés en suspens à la version précédente sont résolus :

1. **Périmètre du seuil de 50 Mo (I1)** : confirmé comme limite souple globale, incluant payload et index. L'index ne pesant que quelques mégaoctets en conditions réelles, il n'affecte pas significativement la borne pratique retenue en I7.
2. **Nature du Dispatcher (Phase 4.3)** : confirmé comme système déjà massivement asynchrone (Tokio/SQLx pour l'orchestration, Axum pour l'API de lecture). La paire `tokio::sync::Semaphore` + `tokio::task::spawn_blocking` est donc la solution actée, sans alternative à évaluer.

Aucun point ouvert restant. Le document est figé pour passage à l'implémentation, Phase 4.1.
