# Spécification : Render Shell

Statut : proposition initiale, prête pour implémentation.
Périmètre : `crates/shell/render/` (moteur) et `crates/shell/server/`
(intégration Axum).
Décisions héritées, non rediscutées ici : ADR-002 (Projection Réactive,
Dispatcher), ADR-006 (chemin de lecture via `sendfile(2)`, zéro calcul
applicatif), ADR-007 (frontière Hot/Cold varlena), ADR-008 (topologie de
l'artéfact de lecture, composition à l'écriture), ADR-009 (pages non
adressables par PK unique → table de synthèse).

---

## 1. Périmètre et invariants contractuels

Le Render Shell est l'unique composant qui touche le réseau côté lecture. Il
ne calcule rien : il résout une route vers un fichier déjà entièrement
composé (ADR-008 §4), localise un fragment dans ce fichier en O(log N), et le
livre. Trois contrats, déjà verrouillés par les ADR, gouvernent tout ce
document :

1. **Lecture pure** — aucune composition, aucun calcul HTML au runtime.
2. **Adressage strict par PK** — `PackfileEntry { id: i64, ... }`, jamais de
   requête composite (ADR-009).
3. **Agnosticisme d'invalidation** — le Render Shell ne connaît jamais la
   relation composant→pages ; il reçoit des ordres d'écriture déjà résolus.

Ce document spécifie quatre mécaniques : le format binaire du packfile HTML
(distinct du `store.bin`), la table de routage générée AOT, le cycle de vie
côté lecture (cold start → hot path), et l'interface d'écriture côté
Dispatcher.

---

## 2. Vue d'ensemble

```
┌─────────────────────────── Écriture (Dispatcher / marius-dump) ───────────┐
│                                                                              │
│  fetch_from_pg/fetch_batch → BatchRenderer<P> → fichier temporaire          │
│       → write_packfile_footer → fsync → rename() atomique                  │
│       → LiveRegistry.swap(nouveau PackHtmlIndex)                           │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────┘
                                    │
                        (le fichier sur disque est la
                         seule frontière entre les deux mondes)
                                    │
┌─────────────────────────── Lecture (Render Shell / Axum) ─────────────────┐
│                                                                              │
│  Cold start : mmap eager de chaque index connu, fd ouverts, madvise        │
│       │                                                                     │
│  Requête HTTP → ROUTE_TABLE (AOT) → LiveRegistry.load() (ArcSwap, lock-free)│
│       → binary_search(id) sur l'index mmap'd → (offset, len)               │
│       → livraison depuis le fd déjà ouvert (voir §6.3)                     │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────┘
```

Deux formats binaires distincts coexistent dans le projet — à ne jamais
confondre :

|              | `store.bin`                                      | packfile HTML (ce document)       |
| ------------ | ------------------------------------------------ | --------------------------------- |
| Contenu      | `StorageRow[]` brutes, `#[repr(C)]`              | Fragments HTML concatenés         |
| Lecteur      | `PackfileReader<P>` (`marius_projection`)        | `PackHtmlIndex` (§5)              |
| Consommateur | `fetch_batch` (relit les données pour re-render) | Render Shell (sert le HTML fini)  |
| Format index | Header en tête (`PackfileStoreHeader`)           | **Footer en fin de fichier** (§3) |

---

## 3. Format binaire du packfile HTML

Le format diffère délibérément de `store.bin` sur un point : **l'index est
un footer, pas un header.** `store.bin` connaît son `row_count` avant
d'écrire (un `Vec` déjà collecté). Le packfile HTML est produit par
`BatchRenderer::render_batch`, conçu pour streamer au fil de chunks
successifs sans connaître la longueur totale du blob par avance — imposer un
header obligerait soit un `Seek` (complexité évitable), soit un double
parcours. Le footer évite les deux : il s'écrit une fois, après le dernier
octet du blob, par construction.

```
Offset 0
  │  HTML blob — fragments concatenés, sans padding, dans l'ordre d'écriture
  │
  ├─ index_start = footer_start - footer.index_len
  │  PackfileEntry[]  — entry_count × 24B, id ASC (hérité de l'ordre de
  │                     dump déjà garanti : SELECT ... ORDER BY id ASC)
  │
  ├─ footer_start = file_len - 32
  │  PackfileFooter   — 32B fixes, toujours les derniers octets du fichier
  ▼
file_len
```

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PackfileEntry {
    pub id:     i64,
    pub offset: u64,
    pub len:    u32,
    pub _pad:   [u8; 4],   // explicite — requis par bytemuck::Pod
}
// 24B, vérifié par const assert.

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PackfileFooter {
    pub magic:       [u8; 8],  // b"MARIUSPK"
    pub version:     u32,
    pub _pad:        [u8; 4],
    pub entry_count: u64,
    pub index_len:   u64,
}
// 32B, vérifié par const assert.
```

Lecture du footer : un seul `mmap`, lire les 32 derniers octets, valider
`magic`/`version`, dériver `index_start` par soustraction — zéro parsing,
zéro syscall supplémentaire. Implémentation livrée dans
`batch_renderer.rs` (`write_packfile_footer`), avec test de round-trip
(`footer_and_index_roundtrip`).

**Invariant non vérifié par le format, à la charge de l'appelant** : l'index
doit être trié par `id` ASC. Hérité sans recalcul de l'ordre déjà garanti
par `dumper.rs` — si cet ordre venait à changer en amont, ce format casserait
silencieusement (`binary_search` sur un slice non trié produit un résultat
indéfini, pas une erreur). À documenter comme précondition critique partout
où ce format est produit.

---

## 4. Table de routage générée AOT

Structure cible, produite par `fragment-forge`/`build.rs` à la compilation —
un slice statique, pas une structure résolue au runtime (ADR-008 §3.2,
`CompositionIndex` reste écarté ; ceci n'en est pas une réincarnation, c'est
une table de faits plats, pas un graphe interrogé) :

```rust
/// D'où vient l'id au moment de la requête.
pub enum IdSource {
    /// Extrait d'un paramètre de route Axum — ex: "/produit/{id}".
    PathParam(&'static str),
    /// Constante — page singleton (ADR-009 : table de synthèse, accueil,
    /// tout artefact dont la "collection" a été résolue en amont par
    /// PostgreSQL, pas par ce composant).
    Fixed(i64),
}

/// Une route connue, résolue à la compilation. Un par template feuille
/// exposé en lecture directe, un par template de page (ADR-008 §4.2).
pub struct RouteEntry {
    /// Pattern Axum, ex: "/produit/{id}", "/admin/produit/{id}", "/".
    pub pattern:      &'static str,
    /// Identifiant stable du packfile à interroger (clé du LiveRegistry,
    /// §5) — réutilise la convention de nommage déjà en vigueur
    /// (`{schema}_{table}_pack.bin` ou `pages_{route}_pack.bin`, ADR-008 §4.4).
    pub packfile_key: &'static str,
    pub id_source:    IdSource,
    pub content_type: &'static str, // ex. "text/html; charset=utf-8", "application/json"
}

/// Table plate, recherche linéaire à l'enregistrement des routes Axum
/// (au démarrage, pas par requête) — dizaines d'entrées attendues, pas
/// besoin d'une structure de recherche dédiée pour cette taille.
pub static ROUTE_TABLE: &[RouteEntry] = &[
    RouteEntry {
        pattern:      "/produit/{id}",
        packfile_key: "pages_product_public",
        id_source:    IdSource::PathParam("id"),
        content_type: "text/html; charset=utf-8",
    },
    RouteEntry {
        pattern:      "/fragment/produit/{id}",
        packfile_key: "commerce_product_core",
        id_source:    IdSource::PathParam("id"),
        content_type: "text/html; charset=utf-8",
    },
    RouteEntry {
        pattern:      "/",
        packfile_key: "pages_homepage",
        id_source:    IdSource::Fixed(1),
        content_type: "text/html; charset=utf-8",
    },
];
```

`ROUTE_TABLE` pilote l'enregistrement des routes Axum au démarrage (une
boucle `for entry in ROUTE_TABLE { router = router.route(entry.pattern, ...) }`)
et fournit, par closure capturée, le `packfile_key` et l'`id_source` au
handler — zéro lookup dynamique de pattern à la requête, Axum a déjà
résolu le pattern matching en amont via son propre routeur.

```
static DUMP_ROUTE_TABLE: &[RouteEntry] = &[RouteEntry {
    pattern:      "/content/:id",
    packfile_key: "content_core",
    id_source:    IdSource::PathParam("id"),
    content_type: "text/html; charset=utf-8",
}];
```

---

## 5. Cold start — initialisation du Render Shell

**Principe directeur : tout mmap se fait au démarrage du processus, jamais
au premier accès.** Un `OnceLock` paresseux (utilisé côté `fetch_batch` pour
`store.bin`, où l'absence de fichier au premier appel est une condition
d'erreur fatale acceptable) serait inapproprié ici : une première requête
HTTP ne doit jamais payer le coût d'un `mmap()` — c'est précisément ce que
"cold start" signifie dans ce contexte : tout le coût d'initialisation est
payé une fois, avant d'accepter la première connexion.

```rust
/// Lecteur d'un packfile HTML — symétrique de PackfileReader<P> (store.bin),
/// format différent (footer, pas header — voir §3).
pub struct PackHtmlIndex {
    file:        std::fs::File,   // fd conservé ouvert — zéro open() par requête.
                                    // Jamais de seek() sur ce fd partagé : toute
                                    // lecture positionnelle passe par read_at
                                    // (pread(2)), voir §6.3 — un seek() sur un fd
                                    // accédé concurremment par plusieurs requêtes
                                    // Tokio est une race condition (le curseur
                                    // d'I/O POSIX est un état partagé mutable).
    mmap:        memmap2::Mmap,    // BORNÉ à la seule région d'index — voir open().
                                    // Le blob HTML n'entre jamais dans l'espace
                                    // d'adressage virtuel de ce processus : un
                                    // Mmap::map(&file) sans bornes mapperait la
                                    // totalité du fichier (réservation de VMA sur
                                    // toute sa taille, même si l'OS ne charge les
                                    // pages qu'à la demande) — contradiction avec
                                    // l'intention si on ne borne pas explicitement.
    entry_count: usize,
}

impl PackHtmlIndex {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::FileExt;

        let file     = std::fs::File::open(path)?;
        let file_len = file.metadata()?.len();

        // Lecture positionnelle du footer (32 derniers octets) via pread —
        // jamais seek()+read(), même au cold start : par cohérence avec
        // l'invariant imposé plus bas sur ce même fd, et parce qu'aucune
        // raison de déroger n'existe ici non plus.
        let footer_start = file_len.checked_sub(32)
            .ok_or_else(|| std::io::Error::other("packfile trop court"))?;
        let mut footer_buf = [0u8; 32];
        file.read_at(&mut footer_buf, footer_start)?;
        let footer: &PackfileFooter = bytemuck::from_bytes(&footer_buf);

        if &footer.magic != b"MARIUSPK" {
            return Err(std::io::Error::other("magic invalide"));
        }
        // ... validation version, cohérence index_len/entry_count (voir §3)

        let index_start = footer_start - footer.index_len;

        // mmap borné exactement à la région d'index — c'est cette borne,
        // pas un commentaire, qui garantit que le blob n'est jamais mappé.
        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .offset(index_start)
                .len(footer.index_len as usize)
                .map(&file)?
        };
        let _ = mmap.advise(memmap2::Advice::WillNeed);   // pré-charge l'index

        Ok(Self { file, mmap, entry_count: footer.entry_count as usize })
    }

    /// Recherche O(log N). Retourne (offset, len) — jamais les octets eux-mêmes,
    /// le blob n'est pas mmap'd (§6.3).
    pub fn lookup(&self, id: i64) -> Option<(u64, u32)> {
        // self.mmap EST déjà la région d'index entière (bornée à l'ouverture) —
        // aucun calcul d'offset supplémentaire nécessaire ici.
        let entries: &[PackfileEntry] = bytemuck::cast_slice(&self.mmap[..]);
        entries.binary_search_by_key(&id, |e| e.id).ok()
            .map(|i| (entries[i].offset, entries[i].len))
    }

    /// Accès au fd partagé pour une lecture positionnelle (§6.3) — jamais pour
    /// un seek() direct.
    pub fn file(&self) -> &std::fs::File {
        &self.file
    }
}
```

**Registre vivant, échangeable sans verrou exclusif :**

```rust
pub struct LiveRegistry {
    indices: std::collections::HashMap<&'static str, arc_swap::ArcSwap<PackHtmlIndex>>,
}

impl LiveRegistry {
    /// Construit le registre en ouvrant et mmap-ant CHAQUE packfile connu
    /// de ROUTE_TABLE — séquence bloquante, exécutée une fois avant
    /// `axum::serve`. Une route dont le packfile est introuvable au
    /// démarrage est une erreur fatale de déploiement (cohérent avec la
    /// doctrine déjà établie pour `fetch_batch` : un artefact absent à ce
    /// stade signale un `marius-dump` manquant, pas une absence de donnée
    /// à tolérer silencieusement).
    pub fn cold_start() -> std::io::Result<Self> {
        let mut indices = std::collections::HashMap::new();
        for entry in ROUTE_TABLE {
            let path  = packfile_path_for(entry.packfile_key);
            let index = PackHtmlIndex::open(&path)?;
            indices.insert(entry.packfile_key, arc_swap::ArcSwap::from_pointee(index));
        }
        Ok(Self { indices })
    }
}
```

---

## 6. Hot path — service d'une requête

### 6.1 Extraction et lookup

```rust
async fn serve_route(
    Path(params):       Path<HashMap<String, String>>,
    State(registry):    State<Arc<LiveRegistry>>,
    Extension(route):   Extension<&'static RouteEntry>,   // injecté par le routeur au montage
) -> impl IntoResponse {
    let id = match route.id_source {
        IdSource::Fixed(n)          => n,
        IdSource::PathParam(name)   => match params.get(name).and_then(|s| s.parse().ok()) {
            Some(id) => id,
            None     => return StatusCode::BAD_REQUEST.into_response(),
        },
    };

    let index_arc = registry.indices[route.packfile_key].load();   // Arc<PackHtmlIndex>, lock-free

    match index_arc.lookup(id) {
        Some((offset, len)) => deliver(index_arc.clone(), offset, len).await,
        None                 => StatusCode::NOT_FOUND.into_response(),
    }
}
```

`registry.indices[...].load()` retourne un `Arc` — la requête en cours
détient sa propre référence, même si le Dispatcher remplace l'entrée en
plein milieu (§7). Aucune contention, aucun verrou exclusif sur le chemin de
lecture.

### 6.2 Capacité gratuite — `Content-Length`

`len` est connu sans calcul (`PackfileEntry.len`, déjà en mémoire) — pas de
`Transfer-Encoding: chunked`, l'en-tête `Content-Length` s'émet directement.

### 6.3 Livraison — note d'implémentation honnête, pas une promesse en l'air

**Point de friction réel, à nommer plutôt qu'à esquiver** : Axum/Hyper
n'exposent pas le descripteur de socket brut à un handler ordinaire — appeler
`libc::sendfile(2)` littéralement depuis l'intérieur d'un handler Axum
classique n'est pas trivial. Deux voies, à choisir explicitement plutôt qu'à
laisser implicite :

**Option A (retenue pour la v1)** — lecture positionnelle via `read_at`
(`pread(2)`, **jamais** `seek()` suivi d'un `read()`). C'est une correction
nécessaire, pas un détail de style : le curseur d'I/O d'un descripteur de
fichier POSIX est un état mutable partagé. Sur le fd unique conservé dans
`PackHtmlIndex` (§5), deux requêtes concurrentes faisant chacune `seek(A)`
puis `read()` peuvent s'entrelacer — la Requête 1 peut lire les octets visés
par la Requête 2 si son `seek()` est immédiatement suivi du `seek()` d'une
autre requête avant que son propre `read()` ne s'exécute. `pread` élimine ce
risque par construction : il prend l'offset en paramètre direct de l'appel
système, sans jamais toucher au curseur partagé du fd.

```rust
async fn deliver(index: Arc<PackHtmlIndex>, offset: u64, len: u32) -> impl IntoResponse {
    use std::os::unix::fs::FileExt;

    // read_at est un appel bloquant (syscall pread) — spawn_blocking évite
    // de geler un worker Tokio pendant l'I/O, même backée par le page cache.
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len as usize];
        index.file().read_at(&mut buf, offset)?;
        Ok(buf)
    }).await;

    match result {
        Ok(Ok(bytes)) => (
            [(axum::http::header::CONTENT_LENGTH, len.to_string())],
            bytes,
        ).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
```

Ceci ne change pas le coût déjà concédé plus haut (une copie userspace par
requête, `Vec<u8>` alloué par appel) — seule la sûreté de l'accès concurrent
au fd partagé change. Le coût d'allocation reste un arbitrage v1 accepté,
pas un oubli : voir la discussion sur l'Option B ci-dessous pour la voie qui
l'élimine.

**Option B (différée)** — `libc::sendfile` réel, via `hyper::upgrade` pour
reprendre la main sur le socket brut après avoir écrit manuellement la ligne
de statut et les en-têtes HTTP, puis restituer la connexion à Hyper pour le
keep-alive. Implémentation correcte mais non triviale (framing HTTP manuel,
gestion d'erreur sur le hand-back). **À ne construire que si l'Option A est
mesurée insuffisante** — même discipline que partout ailleurs dans ce
projet (ADR-007 : ne pas construire le DSL `CHECK` strict avant d'en avoir
besoin ; ADR-008 : ne pas généraliser `CompositionIndex` par anticipation).

Note pour cette voie différée : `sendfile(2)` sous Linux accepte un pointeur
d'offset explicite (`sendfile(out_fd, in_fd, &offset, count)`) — quand cet
offset est non nul, l'appel lit à la position indiquée sans jamais modifier
le curseur partagé du fd source, et met à jour la variable _pointée_ (propre
à l'appelant), pas un état partagé. L'Option B sera donc, elle aussi,
immunisée contre la race condition ci-dessus dès qu'elle sera implémentée
avec ce paramètre — à condition de ne jamais omettre ce pointeur d'offset.

Ce document retient l'Option A (corrigée — `read_at`) comme implémentation
de référence, et nomme l'Option B comme travail différé explicite — pas
comme un détail oublié.

---

## 7. Interface d'écriture — appelée par le Dispatcher

```rust
/// Régénère un packfile HTML complet pour une table/page donnée, et bascule
/// atomiquement le LiveRegistry vers la nouvelle version. Appelée par le
/// Dispatcher (ADR-002, mutation) ou par un outil de dump initial — jamais
/// par le chemin de lecture.
pub async fn regenerate_and_swap<P: Projection>(
    pool:         &sqlx::PgPool,
    ids:          &[i64],              // trié ASC — précondition du format (§3)
    total_cap:    usize,
    packfile_key: &'static str,
    registry:     &LiveRegistry,
) -> std::io::Result<()> {
    let final_path = packfile_path_for(packfile_key);
    let tmp_path    = final_path.with_extension("tmp");

    let file   = std::fs::File::create(&tmp_path)?;
    let mut writer = BufWriter::new(file);

    let mut renderer    = BatchRenderer::<P>::new(total_cap, ids.len().min(CHUNK_SIZE));
    let mut full_index  = Vec::with_capacity(ids.len());
    let mut offset      = 0u64;

    for chunk in ids.chunks(CHUNK_SIZE) {
        let batch = P::fetch_from_pg(pool, chunk).await
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        offset = renderer.render_batch(&batch, &mut writer, offset)?;
        full_index.extend_from_slice(renderer.index());
        renderer.reset(CHUNK_SIZE);
    }

    write_packfile_footer(&mut writer, &full_index)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;   // durabilité avant rename — un crash entre
                                      // les deux ne doit jamais laisser le
                                      // fichier final tronqué.

    std::fs::rename(&tmp_path, &final_path)?;   // atomique (même filesystem, POSIX)

    let new_index = PackHtmlIndex::open(&final_path)?;
    registry.indices[packfile_key].store(Arc::new(new_index));
    // Les requêtes en vol tenant l'ancien Arc terminent sur l'ancienne version —
    // aucune coupure, aucun verrou. Le fichier .tmp renommé libère son inode
    // une fois le dernier Arc (ancien fd+mmap) relâché par le GC de Rust (Drop).

    Ok(())
}
```

**Ce que cette fonction ne fait pas, volontairement** : elle ne sait rien de
_quelles autres pages_ doivent être régénérées en cascade (ADR-008 §5 — la
Forge génère cette relation, le Dispatcher l'interroge avant d'appeler cette
fonction, une fois par cible). Le Render Shell, via cette interface,
n'orchestre jamais une cascade — il exécute une régénération unitaire,
appelée autant de fois que nécessaire par l'appelant.

---

## 8. Invariants vérifiables et tests à prévoir

| Invariant                                                                 | Mécanisme de vérification                                                                                                                                                                                                                                                                                                                                              |
| ------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `PackfileEntry`/`PackfileFooter` tailles fixes                            | `const assert!` (livré, §3)                                                                                                                                                                                                                                                                                                                                            |
| Round-trip footer/index                                                   | Test `footer_and_index_roundtrip` (livré)                                                                                                                                                                                                                                                                                                                              |
| Aucun `seek()` n'est jamais appelé sur le fd partagé d'un `PackHtmlIndex` | Revue de code (`grep -rn "\.seek(" crates/shell/render crates/shell/server` doit ne retourner que des usages sur des `BufWriter`/fichiers temporaires côté écriture, jamais sur `PackHtmlIndex::file()`) ; test de charge concurrente lisant systématiquement des offsets différents en parallèle, assertant que chaque réponse correspond exactement à l'`id` demandé |
| `mmap` d'un `PackHtmlIndex` ne couvre jamais le blob HTML                 | Test ouvrant un packfile synthétique de blob volontairement plus grand que l'index, assertant `mmap.len() == footer.index_len` (pas `file_len`)                                                                                                                                                                                                                        |
| `regenerate_and_swap` atomique vis-à-vis des lecteurs concurrents         | Test d'intégration : lecteur en boucle pendant une régénération, jamais d'erreur ni de lecture partielle                                                                                                                                                                                                                                                               |
| `ROUTE_TABLE` exhaustive vis-à-vis des packfiles réellement présents      | Vérification au cold start (`LiveRegistry::cold_start` échoue fort si absent — §5)                                                                                                                                                                                                                                                                                     |
| Ordre ASC de l'index                                                      | Non vérifié par le format lui-même (limite connue, §3) — à couvrir par un test d'intégration sur un vrai dump, pas par le format binaire                                                                                                                                                                                                                               |

---

## 9. Hors périmètre, explicitement différé

- **`libc::sendfile` réel** (§6.3, Option B) — différé jusqu'à preuve que
  l'Option A est insuffisante.
- **`io_uring`** (mentionné en ADR-008 §3.4) — non engagé.
- **Génération exacte de `ROUTE_TABLE` par `fragment-forge`** — ce document
  spécifie sa forme cible et son usage côté Render Shell, pas l'algorithme de
  `build.rs` qui la produit depuis les templates `.marius` (relève du
  compilateur de templates de page, ADR-008 §4.2/§4.5).
- **Contenu de session/requête** (compteur de panier, etc., ADR-008 §4.3,
  3ᵉ ligne de la taxonomie) — strictement hors du pipeline décrit ici.
