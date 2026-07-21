# StoreRegistry — Document de conception

**Statut** : implémenté (Étapes 1, 3, 4 du Contrat d'Implémentation) — conforme à ce document sauf le point corrigé en §6 (validation avant `rename`, pas après). Conservé comme référence de conception, pas seulement comme document préalable.

**Dépendance amont** : `DFS-phase1-reactivite-cow.md` §3.4 documente désormais le mécanisme réel, en accord avec ce document (à jour au même titre).

**Fichier non vu cette session** : `crates/shell/render/src/registry.rs` (`LiveRegistry`, le registre équivalent existant pour `pack.bin`). Son interface publique est connue par observation indirecte (`regenerate.rs` : `registry.load(key) -> Option<Arc<...>>`, `registry.store(key, Arc<...>)`), pas son implémentation. Les décisions ci-dessous qui s'appuient sur cette interface sont marquées comme telles ; celles qui s'en écartent le sont aussi, explicitement.

---

## 1. Responsabilité

Une seule : garantir que toute lecture de `store.bin` par `fetch_batch` obtient un instantané complet, valide et cohérent — jamais un fichier partiellement écrit, jamais une version dont la validation de header a échoué, jamais un mélange de deux versions au sein d'un même appel.

`StoreRegistry` ne décide *pas* quand une régénération a lieu, ne déclenche *pas* `merge_store`, n'écrit *pas* sur le disque. C'est un point de rendez-vous passif entre un unique producteur (`ingest_and_swap`, un par shard/table à la fois) et un nombre arbitraire de lecteurs concurrents (`fetch_batch`, appelé depuis `fetch_delta_batch`, potentiellement en parallèle inter-shard). Responsabilité strictement bornée à la détention et au remplacement atomique d'un pointeur.

## 2. Forme du composant — décision de conception, pas une extension de `LiveRegistry`

`LiveRegistry` (pack.bin) est interrogé au moment d'une requête HTTP, où la table concernée n'est connue qu'à l'exécution (résolue depuis une route Axum) — d'où une structure à clé de type chaîne (`packfile_key: &'static str`), nécessairement type-érasée ou à indirection dynamique pour porter des `Arc<PackHtmlIndex>` de tables différentes dans une même structure.

`fetch_batch` n'est jamais dans cette situation : il est toujours appelé au travers d'un paramètre de type `P: Projection` **connu à la compilation** — c'est `codegen/projection.rs` qui génère un `fetch_batch` distinct par table, monomorphisé. Il n'y a donc aucun besoin de lookup par clé à l'exécution : une variable statique unique, par table générée, suffit — exactement la même position dans le code que le `static {SCREAMING}_STORE: OnceLock<PackfileReader<P>>` déjà généré aujourd'hui.

**Décision retenue** : `StoreRegistry<P>` n'est pas une structure partagée multi-clés à la manière de `LiveRegistry`. C'est un type générique mono-slot, instancié une fois par `static` générée, remplaçant directement le `OnceLock` existant au même site de génération :

```rust
static {SCREAMING}_STORE: StoreRegistry<{ProjName}Projection> = StoreRegistry::new();
```

Ceci simplifie strictement le problème par rapport à une extension de `LiveRegistry` : pas de `HashMap`, pas de hachage de clé, pas de verrou partagé entre tables sans rapport entre elles. Si cette asymétrie avec `LiveRegistry` s'avère indésirable pour une raison non visible depuis les fichiers audités (ex. `LiveRegistry` porte une responsabilité de cycle de vie globale que je n'ai pas vue faute d'avoir `registry.rs` shell), c'est un point à confirmer avant Contrat d'Implémentation, pas une hypothèse que ce document impose silencieusement.

## 3. API

```rust
pub struct StoreRegistry<P: Projection>
where
    P::Record: Pod,
{
    current: std::sync::RwLock<Option<Arc<PackfileReader<P>>>>,
}

impl<P: Projection> StoreRegistry<P>
where
    P::Record: Pod,
{
    /// État initial : aucune version montée. `const fn` — compatible `static`
    /// (`std::sync::RwLock::new` est `const` depuis Rust 1.63).
    pub const fn new() -> Self;

    /// Provisionnement à froid — appelé une fois, avant toute requête,
    /// jamais en cours de service. Panique si `store.bin` est absent ou
    /// invalide (magic/version/stride/longueur — validations déjà portées
    /// par `PackfileReader::open`) : fail-fast, même discipline que
    /// `regenerate_and_swap` sur `packfile_key` non provisionné.
    pub fn cold_start(&self, path: &Path) -> io::Result<()>;

    /// Lecture — chemin appelé par `fetch_batch`. Acquiert le verrou en
    /// lecture (non contesté en pratique — écriture rare, un `swap` par
    /// tick par shard), clone l'`Arc`, relâche le verrou. Panique si
    /// `cold_start` n'a jamais réussi (invariant AOT : un registre non
    /// provisionné est un bug d'intégration, pas un état à tolérer
    /// silencieusement — cf. §5).
    pub fn load(&self) -> Arc<PackfileReader<P>>;

    /// Écriture — chemin appelé par `ingest_and_swap`, après succès complet
    /// de merge_store + write + fsync + validation du .tmp + rename (cf. §6,
    /// ordre corrigé à l'implémentation : validation avant rename, pas
    /// après). Acquiert le verrou en écriture le temps du remplacement du
    /// pointeur uniquement — jamais pendant l'I/O qui précède.
    pub fn swap(&self, new: Arc<PackfileReader<P>>);
}
```

**Ajout décidé à l'implémentation (Étape 4), absent de la version initiale de ce document** : l'accès à une `StoreRegistry<P>` générée doit être atteignable depuis du code générique `<P: Projection>` (`ingest_and_swap`), qui ne peut pas nommer une `static` monomorphisée pour un `P` inconnu à l'écriture. Solution : `Projection::store_registry() -> &'static StoreRegistry<Self>` comme méthode de trait — pas seulement `cold_start_store()`, restée inhérente puisqu'elle n'a besoin d'être appelée qu'à un site de compilation concret (le bootstrap). Émis par `codegen/projection.rs` aux côtés de `cold_start_store()`.

`std::sync::RwLock<Arc<PackfileReader<P>>>` (état post-`cold_start` ; `RwLock<Option<Arc<...>>>` pour couvrir l'état pré-`cold_start`, cf. §4). **Tranché par `Cargo.toml`** : ni `arc-swap` ni `parking_lot` ne figurent dans `[workspace.dependencies]` — seuls `sqlx`, `tokio`, `rayon`, `axum`, `chrono` y sont déclarés, explicitement présentés comme *« source de vérité unique pour les dépendances communes »*. `crates/core/projection` est classé « Core (no_std attitude) » dans le même fichier — introduire `arc-swap` pour ce seul besoin irait à l'encontre de cette discipline, alors que `std::sync::RwLock` n'ajoute aucune dépendance externe et reste cohérent avec l'usage déjà fait de `std::fs`/`mmap` dans ce même crate (qui n'est donc pas `#![no_std]` au sens strict, mais suit une discipline de dépendances minimales). La contention en écriture est structurellement nulle (un seul `Dispatcher` par shard, un `swap` par tick) : un `RwLock` non contesté en écriture a un coût de lecture négligeable, sans justifier une dépendance supplémentaire.

Réserve non levée par ce seul fichier : `Cargo.toml` racine ne liste que les dépendances **partagées** ; une dépendance ajoutée uniquement au `Cargo.toml` local de `crates/core/projection` (ex. `bytemuck`, déjà utilisé mais absent d'ici, donc probablement local à ce crate) resterait invisible ici. Cela ne change pas la recommandation — `std::sync::RwLock` reste le choix par défaut sans justification d'ajouter quoi que ce soit de nouveau — mais la certitude porte sur « rien n'oblige à ajouter `arc-swap` », pas sur « `arc-swap` est absent de tout le projet ».

## 4. Invariants

- **INV-1** : `load()` ne retourne jamais un `PackfileReader` dont l'ouverture n'a pas réussi la validation complète du header (`PackfileReader::open`, `lib.rs` l.192-253) — un `swap()` ne peut recevoir qu'un `Arc` déjà construit par un `open()` réussi ; l'API ne permet pas de construire un état invalide.
- **INV-2** : une fois `cold_start` réussi, le registre n'est plus jamais vide — `swap()` remplace, ne retire jamais. Un état « non provisionné » n'existe qu'avant le premier `cold_start` réussi et doit rester un cas fatal (`panic!`), jamais un `Option` silencieusement propagé jusqu'au hot path.
- **INV-3** : un `Arc<PackfileReader<P>>` obtenu par un `load()` reste valide et cohérent pendant toute sa durée de vie chez l'appelant, y compris si un ou plusieurs `swap()` surviennent pendant ce temps — garanti par le comptage de références `Arc` (le `Drop` du dernier détenteur libère le `mmap`, pas le `swap()` lui-même) et par la sémantique POSIX de `rename()` (un `mmap` reste valide sur l'inode d'origine même après que le chemin ait été réassigné à un nouveau fichier — propriété déjà exploitée implicitement par `LiveRegistry`/`pack.bin`, pas une garantie nouvelle inventée ici).
- **INV-4** : un seul `swap()` en vol à la fois par instance de `StoreRegistry<P>` — garanti par construction (un seul `Dispatcher` actif par shard/table), pas par un verrou interne au registre. Le registre lui-même reste correct même sous `swap()` concurrents (dernière écriture gagne, atomique), mais cette situation ne devrait jamais se produire ; si elle se produisait, ce serait un bug d'orchestration en amont, pas une défaillance du registre.

## 5. Cycle de vie

1. **Déclaration** : `static` générée par `codegen/projection.rs`, initialisée par `StoreRegistry::new()` — état vide, coût nul, compatible `const fn`/`static` (aucune allocation avant le premier `cold_start`).
2. **Provisionnement** : au démarrage du serveur (`main.rs`, avant tout `tokio::spawn` de `Dispatcher` ou tout service Axum), appel explicite de `cold_start(path)` pour chaque `Projection` générée. Échec fatal si `store.bin` est absent (aucun `marius-dump` n'a jamais eu lieu pour cette table) — même discipline fail-fast que le reste du système AOT, pas de démarrage partiel silencieux.
3. **Service** : `load()` appelé à chaque `fetch_batch`, potentiellement plusieurs fois par seconde par shard, en concurrence inter-shard (Tokio). Coût : un clone atomique d'`Arc`, pas de syscall, pas d'allocation hors du compteur de référence.
4. **Mutation** : `swap()` appelé exactly once par cycle `ingest_and_swap` réussi — jamais en cas d'échec à une étape antérieure (cf. §6).
5. **Fin de vie** : jamais explicite. Le registre vit aussi longtemps que le processus. Le dernier `Arc` (ancienne version, après un `swap`) est libéré dès que le dernier lecteur qui le détenait encore le relâche — potentiellement bien après le `swap()` lui-même si un `fetch_batch` était en cours au moment du remplacement (cf. INV-3).

## 6. Ce qui se passe pendant un `swap`

**Corrigé à l'implémentation (Étape 4)** — la séquence ci-dessous inverse l'ordre initialement prévu par ce document (validation *après* `rename`). Correction, pas une divergence tolérée : sur un `rename` intra-filesystem (garanti par construction, `.tmp` et le fichier final partagent le même répertoire), `rename()` est une opération de métadonnées pure — un fichier qui valide avant `rename` valide identiquement après. Valider *avant* élimine toute fenêtre, même théorique, où un fichier non validé pourrait porter le chemin canonique — y compris en cas de crash entre les deux opérations. Bénéfice supplémentaire : le handle de lecture obtenu par cette validation est directement réutilisé pour le `swap()`, sans second `open()`.

Le `swap()` lui-même reste instantané et atomique (une écriture de pointeur). Ce qui *précède*, côté `ingest_and_swap`, porte la responsabilité de sûreté :

1. `merge_store` produit le contenu fusionné (§3.3, DFS).
2. Écriture dans `store.bin.tmp`, `fsync`.
3. **Validation** : `PackfileReader::open(&tmp_path)` — sur le `.tmp`, avant tout `rename`. Revalide magic/version/stride/longueur, exactement les mêmes contrôles que n'importe quel montage à froid. Échec ⇒ suppression best-effort du `.tmp`, retour d'erreur, `store.bin`/registre strictement inchangés.
4. `rename()` atomique OS : `store.bin.tmp → store.bin`. L'inode ne change pas — le handle obtenu à l'étape 3 reste valide et correct après cette opération.
5. `swap()` du handle déjà validé (pas une réouverture) — seulement si les étapes 3 et 4 ont réussi. Un échec à l'une ou l'autre est une erreur fatale de tick (log + abandon de ce cycle, ancienne version conservée) — jamais un `swap()` partiel.

Aucun lecteur en cours d'un `load()` précédent n'est affecté : il détient déjà son `Arc`, indépendant du remplacement. Aucun nouveau `load()` déclenché après l'étape 5 ne peut voir l'ancienne version.

## 7. Garanties de concurrence

- **Lecture non bloquante en pratique, pas lock-free au sens strict** : `load()` acquiert un verrou de lecture `std::sync::RwLock` — plusieurs lecteurs concurrents ne se bloquent jamais entre eux ; un lecteur ne peut être retardé que par un `swap()` en cours, lui-même borné à la durée d'un remplacement de pointeur (pas de l'I/O qui le précède, cf. §6). Non lock-free au sens strict (littéralement zéro instruction de synchronisation), mais sans contention réelle vu la fréquence d'écriture (un `swap` par tick par shard, potentiellement quelques Hz).
- **Écriture quasi non bloquante pour les lecteurs** : `swap()` ne retient le verrou d'écriture que le temps d'un remplacement de pointeur — jamais pendant `merge_store`, l'écriture disque, le `fsync` ou le `rename`, qui ont tous lieu avant l'acquisition du verrou.
- **Cohérence par appel, garantie et verrouillée** : `fetch_batch` effectue un **unique** `load()` en tête d'appel, et réutilise ce même `Arc<PackfileReader<P>>` pour la résolution de tous les ids du batch — jamais un `load()` par id, jamais un rechargement en cours de boucle. Un batch représente un instantané logique unique de `store.bin` ; il ne doit jamais pouvoir mélanger deux générations, même si un `swap()` survient pendant son traitement. C'est un invariant du code généré (`codegen/projection.rs`), pas seulement une recommandation — il conditionne directement la forme de la boucle de `fetch_batch` au moment du Contrat d'Implémentation : le `load()` doit apparaître une fois, avant la boucle sur `ids`, jamais à l'intérieur.

**INV-5** (ajouté aux invariants du §4) : pour un même appel à `fetch_batch`, tous les `record`/`VarlenOwned` résolus proviennent d'un seul et même `Arc<PackfileReader<P>>` — jamais de deux générations différentes au sein d'un même batch.

**Point traité comme audit séparé, hors périmètre de ce document et de la DFS** : `fetch_batch` est-il appelé uniquement depuis `regenerate_and_swap`/`fetch_delta_batch` (chemin de régénération), ou existe-t-il un chemin HTTP direct qui l'invoquerait aussi ? Aucun fichier vu cette session (`handlers.rs` non fourni) ne permet de trancher. Si un tel chemin existe, ce n'est pas un fait qui modifierait cette conception ou la DFS — l'architecture cible (chemin chaud = `pread` sur `pack.bin` uniquement, jamais `fetch_batch`) reste la référence. Un appel HTTP direct à `fetch_batch` serait à traiter comme une **divergence d'implémentation** à corriger, au même titre que le `OnceLock`/`store.bin` périmé déjà identifié — pas comme une contrainte supplémentaire à absorber dans ce design.

## 8. Pourquoi un `OnceLock` n'est-il plus suffisant ?

`OnceLock::get_or_init` n'exécute son initialiseur qu'une seule fois pour toute la durée de vie du programme, par construction — il n'existe aucune API pour remplacer la valeur après le premier montage. C'était cohérent avec le modèle (erroné) où `store.bin` était monté une fois et jamais rafraîchi en cours de service. Maintenant que `store.bin` doit refléter un delta à chaque tick, le point de montage doit être remplaçable — ce n'est pas une limitation de `OnceLock` à contourner, c'est la mauvaise primitive pour une valeur qui varie dans le temps. `std::sync::RwLock` (retenu, §3) est la primitive std conçue pour ce problème précis, sans dépendance externe.

## 9. Pourquoi ne pas simplement rouvrir le fichier à chaque appel ?

Trois raisons, indépendantes :

1. **Coût syscall/mmap répété** — `open()` + `mmap()` + `madvise(MADV_WILLNEED)` à chaque `fetch_batch`, potentiellement plusieurs fois par seconde par shard, annule l'investissement explicite déjà fait par le `madvise` actuel (`lib.rs` l.186-198, commentaire : *« élimine les page faults lors des premiers lookups »* — un hint payé une fois au montage, pas destiné à être répété).
2. **Fenêtre de lecture d'un fichier en cours d'écriture** — sans registre, chaque appelant devrait lui-même décider quand `store.bin` est stable avant de le mmaper ; le registre centralise cette décision (ne publier une version qu'après rename + revalidation, §6) au lieu de la dupliquer dans chaque site d'appel.
3. **Perte de la cohérence intra-appel** — deux `open()` indépendants au sein d'un même `fetch_batch` (par exemple pour deux chunks consécutifs) pourraient chacun mmaper une version différente si un `swap` survient entre les deux ouvertures — un `record` et son `VarlenOwned` associé pourraient alors provenir de deux générations différentes de `store.bin`. Un seul `load()` en tête d'appel élimine structurellement ce risque (cf. §7, INV-5).

---

## Ce que ce document ne tranche pas

- L'existence ou non d'un chemin HTTP appelant `fetch_batch` directement — audit séparé, sans impact sur cette conception ni sur la DFS (§7).
