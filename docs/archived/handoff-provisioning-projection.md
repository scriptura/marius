**Handoff d'implémentation — Provisioning de l'espace de projection (sous-phase de l'Orchestration Globale `main.rs`)**

Tu es consulté en tant qu'Expert en Ingénierie Système Haute Performance (Rust, DOD, Mechanical Sympathy, Zero-Copy, Tokio).

À joindre obligatoirement en pièce de contexte avant de commencer : `specification-provisioning-projection.md` (architecture complète, arbitrages tranchés, audit du code source déjà effectué). Ce handoff n'en répète pas le raisonnement — il en extrait uniquement ce qui est nécessaire à l'exécution. En cas de divergence entre ce handoff et la spec, **la spec fait foi** ; signale la divergence plutôt que de trancher silencieusement.

**Cette session ne couvre que le provisioning** : faire en sorte qu'un démarrage sur environnement vierge (aucun fichier sous `artifacts/`) ne produise plus l'erreur fatale `cold_start: échec ouverture packfile ... No such file or directory`, sans toucher au comportement existant pour tout le reste.

---

## Arbitrage architecte — résumé, cadre figé (détail complet : spec jointe)

1. **Classification à trois branches** : absent → provisionner (créer un packfile vide valide) ; présent valide → charger normalement ; présent invalide/corrompu → fatal immédiat, inchangé. Confirmé par lecture de `pack_html_index.rs` : `PackHtmlIndex::open` distingue déjà `io::ErrorKind::NotFound` (absence) de `io::Error::other(...)` (toute corruption, y compris un fichier vide ou tronqué) — la troisième branche du tableau n'exige donc aucun code supplémentaire, elle existe déjà.
2. **Aucune connaissance du format binaire dans `main.rs`** : un seul point d'appel nommé pour l'intention (`ensure_provisioned`), exposé par `marius-render`.
3. **Un seul écrivain du format** : la primitive de sérialisation est `pack_html_format::write_packfile_footer(writer, blob_len, index)`, déjà publique, déjà désignée source de vérité unique par son propre en-tête, déjà consommée en production par `batch_renderer.rs`. Le provisioning l'appelle avec `blob_len = 0` et `index = &[]` — cas générique de la fonction existante, pas une branche ajoutée pour l'occasion. Aucun refactor de `regenerate.rs` n'est nécessaire.
4. **Pas de `trait ProjectionStorage`/`PackfileStore`** : décision actée, pas une option à reconsidérer. Une fonction libre, monomorphisée sur rien (pas de `Projection`), homogène sur `&'static str`.
5. **Discipline `spawn_blocking` inconditionnelle** : confirmée transverse au système par trois sites indépendants (`regenerate_and_swap`/`apply_merge_io_sync`, `handlers.rs`/`deliver`). Le corps synchrone du provisioning doit être déporté via `tokio::task::spawn_blocking`, même si rien ne sert encore au moment de son exécution — la règle ne dépend pas du contexte de contention, elle est inconditionnelle dès qu'un appel système bloquant est en jeu en contexte async.

---

## Référence — état actuel exact (post Phase 5.3, livré)

### `main.rs`, début de `async fn main()` — extrait pertinent

```rust
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Ressources globales (spec §4) ───────────────────────────────────────
    let database_url = std::env::var("DATABASE_URL")?;
    let pool = sqlx::PgPool::connect(&database_url).await?;
    eprintln!("[marius-server] PgPool connecté"); // jamais l'URL en clair (identifiants)

    let io_permits: usize = std::env::var("MARIUS_IO_PERMITS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(4);
    let io_semaphore = Arc::new(Semaphore::new(io_permits));
    eprintln!("[marius-server] Arc<Semaphore> initialisé — {io_permits} permis I/O");

    // Cold start : mmap eager de chaque index connu, fd ouverts — tout le
    // coût d'initialisation payé une fois, avant d'accepter la première
    // connexion (spec §5/Phase 3). Échec fatal si un packfile référencé par
    // ROUTE_TABLE est introuvable — pas de dégradation silencieuse.
    let registry = Arc::new(LiveRegistry::cold_start(ROUTE_TABLE)?);
    eprintln!(
        "[marius-server] cold_start réussi — {} route(s) enregistrée(s)",
        ROUTE_TABLE.len()
    );

    // ── Dispatcher — shard content_core ─────────────────────────────────────
    // [...] (Phase 5.1-5.3, inchangé par cette session)
```

`ROUTE_TABLE: &[RouteEntry]` expose `.packfile_key: &'static str` (confirmé, déjà utilisé ailleurs dans `main.rs`, y compris dans ses propres tests via `marius_render::packfile_path_for`).

### `pack_html_format.rs` — primitive de sérialisation déjà existante (inchangée par cette session)

```rust
pub fn write_packfile_footer<W: Write>(
    writer: &mut BufWriter<W>,
    blob_len: u64,
    index: &[PackfileEntry],
) -> std::io::Result<()> { /* ... déjà livrée, ne pas modifier ... */ }
```

### `regenerate.rs` — `apply_merge_io_sync`, idiome d'écriture atomique à reproduire (tmp + fsync + rename), **pas à appeler directement** (exige un `old: &PackHtmlIndex` déjà ouvert — non pertinent pour le cas vierge, cf. spec §4)

---

## Mission

### 1. Correctif `registry.rs` — isolation de `packfile_path_for` pour le test de bout en bout

Référence actuelle, confirmée (`crates/shell/render/src/registry.rs`, lignes 84-86) :

```rust
pub fn packfile_path_for(packfile_key: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("artifacts").join(format!("{packfile_key}.bin"))
}
```

Aucun appelant existant ne doit être touché par ce correctif — signature strictement préservée. Ajout d'une indirection par variable d'environnement, lue une seule fois (`OnceLock`, même discipline DOD que `panic_on_first_tick`, Phase 5.3 — compute once, branche gratuite ensuite), suivant exactement le mécanisme déjà établi trois fois dans ce système (`MARIUS_DEBUG_PANIC_SHARD`, `MARIUS_BIND`, `MARIUS_IO_PERMITS`) — aucun second mécanisme de configuration introduit, juste une nouvelle variable du même type :

```rust
pub fn packfile_path_for(packfile_key: &str) -> std::path::PathBuf {
    static ARTIFACTS_DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let base = ARTIFACTS_DIR.get_or_init(|| {
        std::env::var("MARIUS_ARTIFACTS_DIR").unwrap_or_else(|_| "artifacts".to_string())
    });
    std::path::PathBuf::from(base).join(format!("{packfile_key}.bin"))
}
```

Comportement en production strictement inchangé : variable absente → `"artifacts"`, valeur de retour identique octet pour octet à l'implémentation actuelle, pour tout appelant existant (`cold_start`, voie d'écriture réactive, tests déjà présents dans `main.rs`). Le `OnceLock` est sûr pour le test de bout en bout ci-dessous précisément parce que celui-ci s'exécute en sous-processus (`Command::new(CARGO_BIN_EXE_marius)`) — chaque sous-processus reçoit un `OnceLock` vierge, donc aucune contamination entre tests via une valeur mise en cache par un test voisin dans le même binaire. Ne pas réutiliser ce mécanisme pour un test in-process qui ferait varier la variable plusieurs fois dans le même process : le cache piégerait un tel usage — non pertinent ici, à signaler si un futur besoin s'y heurte.

### 2. Nouvelle fonction dans `marius-render` — provisioning

Emplacement suggéré : `regenerate.rs`, voisine d'`apply_merge_io_sync` (même idiome d'écriture atomique, même fichier responsable de la durabilité disque). Si la cohésion du module suggère plutôt `pack_html_format.rs` une fois le code sous les yeux, trancher sur place et documenter le choix en commentaire — la spec ne fige pas le fichier exact, seulement l'absence de duplication de format (§3 de l'arbitrage ci-dessus).

```rust
// Corps synchrone — ne connaît qu'un chemin, pas de PgPool ni de LiveRegistry.
fn ensure_provisioned_sync(packfile_key: &'static str) -> std::io::Result<ProvisionOutcome> {
    let final_path = packfile_path_for(packfile_key);
    match std::fs::metadata(&final_path) {
        Ok(_) => Ok(ProvisionOutcome::AlreadyPresent),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let tmp_path = final_path.with_extension("tmp");
            if let Some(parent) = tmp_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .write(true).create(true).truncate(true)
                .open(&tmp_path)?;
            let mut writer = std::io::BufWriter::new(file);
            write_packfile_footer(&mut writer, 0, &[])?; // blob vide, index vide
            writer.flush()?;
            writer.into_inner().map_err(std::io::Error::other)?.sync_all()?;
            std::fs::rename(tmp_path, final_path)?;
            Ok(ProvisionOutcome::Provisioned)
        }
        Err(e) => Err(e), // tout le reste reste fatal
    }
}

/// Idempotent : sans effet si le packfile existe déjà (quel que soit son
/// contenu — cold_start() qualifiera sa validité ensuite, ce n'est pas le
/// rôle de cette fonction). N'écrit jamais le format directement : délègue
/// entièrement à write_packfile_footer (pack_html_format.rs), seule source
/// de vérité du format on-disk.
pub async fn ensure_provisioned(packfile_key: &'static str) -> std::io::Result<ProvisionOutcome> {
    tokio::task::spawn_blocking(move || ensure_provisioned_sync(packfile_key))
        .await
        .map_err(std::io::Error::other)?
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionOutcome {
    AlreadyPresent,
    Provisioned,
}
```

Exporter `ensure_provisioned` et `ProvisionOutcome` depuis `lib.rs` du crate `marius-render`, en suivant exactement la convention déjà établie pour `regenerate_and_swap` (ré-export à plat d'une fonction principale de module, avec le commentaire qui en justifie la cohérence) — confirmé par lecture directe de `lib.rs` :

```rust
// ensure_provisioned, ProvisionOutcome — même convention que
// regenerate_and_swap ci-dessus (fonction/type principal d'un module,
// ré-exporté à plat). main.rs l'appelle via marius_render::ensure_provisioned,
// pas marius_render::regenerate::ensure_provisioned.
pub use regenerate::{ensure_provisioned, ProvisionOutcome};
```

(en supposant l'emplacement `regenerate.rs` retenu à l'étape 1 ci-dessus ; adapter le chemin de ré-export si `pack_html_format.rs` est choisi à la place — la convention reste la même dans les deux cas.)

### 3. Câblage dans `main.rs`

Insertion entre l'initialisation du semaphore et l'appel à `cold_start` (juste avant la ligne `let registry = Arc::new(LiveRegistry::cold_start(ROUTE_TABLE)?);` citée en référence ci-dessus) :

```rust
    // ── Provisioning de l'espace de projection ──────────────────────────────
    // Un environnement vierge (aucun fichier sous artifacts/) n'est pas une
    // erreur : c'est l'état initial légitime d'un espace de projection pas
    // encore matérialisé (spec-provisioning §1). Tout le reste — packfile
    // présent mais corrompu — reste fatal via cold_start ci-dessous, inchangé.
    for route in ROUTE_TABLE {
        match marius_render::ensure_provisioned(route.packfile_key).await? {
            marius_render::ProvisionOutcome::Provisioned => eprintln!(
                "[marius-server] espace de projection provisionné (vierge) — shard \"{}\"",
                route.packfile_key
            ),
            marius_render::ProvisionOutcome::AlreadyPresent => {}
        }
    }
```

`LiveRegistry::cold_start(ROUTE_TABLE)?` reste **strictement inchangé** — aucune ligne touchée dans `registry.rs`. Le reste de `main()` (Dispatchers, `PgListener`, `JoinSet`, `axum::serve`, supervision fail-fast — Phase 5.3, livrée) reste inchangé.

### 4. Tests

Deux niveaux, dans la continuité de la discipline déjà établie (Phase 5.3 : in-process pour les contrats isolés, sous-processus/fixtures réelles pour le comportement de boot) :

- **Tests unitaires de `ensure_provisioned`** (dans `regenerate.rs` ou `pack_html_format.rs`, selon l'emplacement choisi à l'étape 1), `#[tokio::test]` :
  - `ensure_provisioned` sur un chemin inexistant → `Provisioned`, fichier créé, et **`PackHtmlIndex::open()` sur ce fichier réussit** avec `entry_count() == 0` — preuve que le fichier produit est valide selon le lecteur réel, pas seulement selon l'intuition de l'auteur de l'écrivain.
  - `ensure_provisioned` sur un chemin déjà présent (fixture quelconque, même invalide) → `AlreadyPresent`, **fichier non modifié** (vérifier mtime ou contenu inchangé) — la fonction ne doit jamais écraser un fichier existant, même corrompu : ce n'est pas son rôle de qualifier la validité.
  - Idempotence : deux appels successifs sur un chemin initialement absent → `Provisioned` puis `AlreadyPresent`, fichier identique après les deux appels.
- **Test de bout en bout** (fichier d'intégration existant ou nouveau, à la discrétion de l'implémenteur — suivre la convention déjà établie en Phase 5.3 de séparation sous-processus/in-process), `#[test]` synchrone, même patron que `fail_fast_panic_in_dispatcher_terminates_process` : créer un répertoire temporaire vide (`tempfile::tempdir()` ou équivalent déjà disponible dans les dépendances de test du workspace — à confirmer), démarrer `Command::new(env!("CARGO_BIN_EXE_marius"))` avec `MARIUS_ARTIFACTS_DIR` pointant vers ce répertoire (en plus de `DATABASE_URL`, `MARIUS_BIND` éphémère comme en Phase 5.3) → le processus démarre jusqu'au bout sans erreur fatale (polling borné sur `try_wait()`, même discipline que Phase 5.3), **et** une requête HTTP sur une route provisionnée vide répond 404 (pas 500) — conséquence déjà confirmée par audit de `handlers.rs` (`lookup()` sur `entry_count: 0` retombe sur la branche `NOT_FOUND` existante, zéro changement requis côté Read Path). Le répertoire temporaire garantit l'isolation totale vis-à-vis des fixtures réelles sous `artifacts/` — aucun déplacement/sauvegarde de fichiers existants nécessaire, grâce au Correctif de l'étape 1.

---

## Prérequis à vérifier avant de démarrer cette session (bloquant — environnement, pas code)

- Identiques aux phases précédentes : `DATABASE_URL` valide, migrations appliquées. L'isolation du test de bout en bout (répertoire `artifacts/` vide) est désormais garantie par construction via `MARIUS_ARTIFACTS_DIR` (Correctif étape 1) — plus une zone d'incertitude environnementale.

**Hors scope explicite, à ne pas aborder ici** (cf. spec §6) :

- Reconstruction de données préexistantes (`marius-dump`) — un packfile absent est provisionné vide, jamais reconstruit à partir de lignes Postgres existantes.
- Cascade de régénération de `pages_homepage` (ADR-008, différée) — son packfile est provisionné vide comme les deux autres, sans que sa cascade soit implémentée.
- Nettoyage de fichiers `.tmp` orphelins issus d'un crash entre `write` et `rename` — hors périmètre, question d'hygiène disque séparée.
- Toute modification de `cold_start`, `PackHtmlIndex::open`, `apply_merge_io_sync`, `write_packfile_footer` — tous intouchés, le provisioning ne fait qu'appeler le dernier.

## Tests / Jalon — critères d'acceptation

- `cargo build`/`clippy` propres ; suites Jalon 3, 5.2, 5.3 existantes toujours vertes.
- Démarrage sur `artifacts/` vide : aucune erreur fatale, les trois entrées de `ROUTE_TABLE` provisionnées, `cold_start` les charge sans branche spéciale (déjà garanti par construction, à confirmer par le test de bout en bout).
- Démarrage avec packfiles déjà présents et valides (cas actuel) : comportement strictement inchangé — `ensure_provisioned` no-op sur les trois entrées, zéro écriture disque.
- Un packfile présent mais corrompu/tronqué continue de provoquer un arrêt fatal immédiat — vérifier qu'aucun chemin de `ensure_provisioned` ne masque ce cas (il ne le devrait pas, par construction : seul `NotFound` déclenche l'écriture).
- Aucun appel direct à `write_packfile_footer` ni à `pack_html_format` depuis `main.rs` — un seul point d'entrée (`ensure_provisioned`).
- `packfile_path_for` retourne une valeur strictement identique à l'implémentation actuelle pour tout appelant existant tant que `MARIUS_ARTIFACTS_DIR` n'est pas positionnée — non-régression à vérifier explicitement, pas seulement supposée du fait de la relecture de code.
