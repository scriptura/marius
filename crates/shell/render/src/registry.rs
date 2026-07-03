// =============================================================================
// crates/shell/render/src/registry.rs
//
// Registre vivant des PackHtmlIndex — specification-marius-render-shell.md
// §5. Un ArcSwap<PackHtmlIndex> par packfile_key, jamais un singleton :
// chaque route (ROUTE_TABLE, Phase 3) cible un packfile_key distinct, chacun
// remplaçable indépendamment des autres. Tranché par lecture directe de la
// spec §5 (HashMap<&'static str, ArcSwap<PackHtmlIndex>>), pas par
// supposition — cf. handoff-render-shell-phase2.md.
//
// Invariant AOT structurant : la topologie des clés est figée à la
// construction. Aucun insert()/remove() sur `indices` après with_indices() —
// seuls les ArcSwap qu'elle contient sont mutés (store()). C'est cette
// immutabilité de la table elle-même, pas un verrou, qui rend l'accès
// concurrent sûr. D'où le champ `indices` privé et les méthodes
// load()/store() enveloppantes ci-dessous — encapsulation retenue en Phase 2,
// conservée telle quelle en Phase 3 (handlers.rs de marius-server passe
// systématiquement par load()/store(), jamais par un accès direct au champ).
//
// cold_start() (Phase 3) — frontière de crate tranchée en session
// (handoff-render-shell-phase3.md) : la spec §5 écrit `for entry in
// ROUTE_TABLE { ... }` comme accès global implicite, mais ROUTE_TABLE est
// écrite à la main dans crates/shell/server/src/main.rs (marius-server),
// un crate qui DÉPEND de marius-render — jamais l'inverse (le sens contraire
// créerait un cycle de dépendances, impossibilité structurelle du workspace,
// pas un choix de style). Résolution retenue : cold_start() prend la table
// en PARAMÈTRE (&'static [RouteEntry]), jamais comme global lu depuis ce
// crate. RouteEntry/IdSource vivent donc ici (marius-render) — marius-server
// les importe pour construire sa propre ROUTE_TABLE, pas l'inverse.
// =============================================================================

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::pack_html_index::PackHtmlIndex;

// =============================================================================
// Types de routage AOT — specification-marius-render-shell.md §4.
//
// Définis dans marius-render (pas marius-server) pour que cold_start()
// puisse les manipuler sans dépendance inverse — voir note de module
// ci-dessus. La ROUTE_TABLE elle-même (les valeurs) reste écrite à la main
// dans crates/shell/server/src/main.rs : seuls les TYPES sont ici. Le
// compilateur de templates de pages (FragmentRef, ADR-008 §4.2-§4.5) qui
// générerait cette table n'existe pas — hors périmètre de cette roadmap.
// =============================================================================

/// D'où vient l'id au moment de la requête (spec §4).
#[derive(Debug, Clone, Copy)]
pub enum IdSource {
    /// Extrait d'un paramètre de route Axum — ex: "/produit/:id".
    PathParam(&'static str),
    /// Constante — page singleton (ADR-009 : artefact dont la "collection"
    /// a déjà été résolue en amont par PostgreSQL, pas par ce composant).
    Fixed(i64),
}

/// Une route connue, résolue à la compilation. Un par template feuille
/// exposé en lecture directe, un par template de page (ADR-008 §4.2).
#[derive(Debug, Clone, Copy)]
pub struct RouteEntry {
    /// Pattern Axum, ex: "/produit/:id", "/".
    pub pattern: &'static str,
    /// Identifiant stable du packfile à interroger — clé du LiveRegistry.
    /// Plusieurs RouteEntry peuvent partager la même clé (ex: page complète
    /// + fragment HTMX du même contenu) — cold_start() déduplique.
    pub packfile_key: &'static str,
    pub id_source: IdSource,
}

/// Résout le chemin disque d'un packfile à partir de sa clé.
///
/// Contrat de base inchangé depuis sa définition initiale
/// (`specification-marius-render-shell.md` §5/§7) :
/// `{base}/{packfile_key}.bin`, `base` valant `"artifacts"` par défaut,
/// relatif au répertoire de travail du processus.
///
/// Indirection ajoutée (spec-provisioning-projection.md, handoff étape 1) :
/// `base` est lu une seule fois depuis la variable d'environnement
/// `MARIUS_ARTIFACTS_DIR` (`OnceLock`, même discipline DOD que
/// `panic_on_first_tick`, Phase 5.3 — compute once, branche gratuite
/// ensuite), suivant exactement le mécanisme déjà établi trois fois dans ce
/// système (`MARIUS_DEBUG_PANIC_SHARD`, `MARIUS_BIND`, `MARIUS_IO_PERMITS`)
/// — aucun second mécanisme de configuration introduit.
///
/// Comportement en production strictement inchangé : variable absente →
/// `"artifacts"`, valeur de retour identique octet pour octet à
/// l'implémentation d'origine, pour tout appelant existant (`cold_start`,
/// voie d'écriture réactive, tests ci-dessous). Le `OnceLock` est sûr pour
/// le test de bout en bout du provisioning (phase5_3_supervision.rs)
/// précisément parce que celui-ci s'exécute en sous-processus — chaque
/// sous-processus reçoit un `OnceLock` vierge, aucune contamination entre
/// tests via une valeur mise en cache par un voisin du même binaire. Ne pas
/// réutiliser ce mécanisme pour un test in-process qui ferait varier la
/// variable plusieurs fois dans le même process : le cache piégerait un tel
/// usage.
pub fn packfile_path_for(packfile_key: &str) -> std::path::PathBuf {
    static ARTIFACTS_DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let base = ARTIFACTS_DIR.get_or_init(|| {
        std::env::var("MARIUS_ARTIFACTS_DIR").unwrap_or_else(|_| "artifacts".to_string())
    });
    std::path::PathBuf::from(base).join(format!("{packfile_key}.bin"))
}

/// Registre vivant des index de packfiles HTML — un `ArcSwap<PackHtmlIndex>`
/// par `packfile_key`, échangeable sans verrou exclusif.
pub struct LiveRegistry {
    /// Topologie figée après construction. Privé : tout accès passe par
    /// load()/store() ci-dessous.
    indices: HashMap<&'static str, ArcSwap<PackHtmlIndex>>,
}

impl LiveRegistry {
    /// Construit le registre à partir d'une table déjà peuplée.
    ///
    /// Conservé pour les pilotes de test qui veulent peupler le registre
    /// sans passer par cold_start()/ROUTE_TABLE (Jalon 2) — cold_start()
    /// ci-dessous est désormais le constructeur de production (Phase 3).
    pub fn with_indices(indices: HashMap<&'static str, ArcSwap<PackHtmlIndex>>) -> Self {
        Self { indices }
    }

    /// Construit le registre en ouvrant et mmap-ant CHAQUE packfile
    /// référencé par `route_table` — séquence bloquante, exécutée une fois
    /// avant `axum::serve` (spec §5, "tout mmap se fait au démarrage du
    /// processus, jamais au premier accès").
    ///
    /// `route_table` est injecté en paramètre, jamais lu comme global
    /// implicite — voir note de module sur la frontière de crate.
    ///
    /// Une route dont le packfile est introuvable au démarrage est une
    /// erreur fatale de déploiement (le `?` propage l'échec de
    /// `PackHtmlIndex::open`, enrichi de contexte) — même discipline que
    /// `fetch_batch` face à un artefact `store.bin` absent : un
    /// `marius-dump` manquant à ce stade ne se tolère pas silencieusement.
    ///
    /// Déduplication explicite par `Entry` : plusieurs `RouteEntry`
    /// partageant le même `packfile_key` (page complète + fragment HTMX, par
    /// exemple) ne déclenchent qu'un seul `open()`/`mmap()` du fichier
    /// physique correspondant.
    pub fn cold_start(route_table: &'static [RouteEntry]) -> std::io::Result<Self> {
        let mut indices: HashMap<&'static str, ArcSwap<PackHtmlIndex>> = HashMap::new();

        for entry in route_table {
            if let Entry::Vacant(slot) = indices.entry(entry.packfile_key) {
                let path = packfile_path_for(entry.packfile_key);
                let index = PackHtmlIndex::open(&path).map_err(|e| {
                    std::io::Error::other(format!(
                        "cold_start: échec ouverture packfile \"{}\" (route \"{}\", \
                         chemin {}) : {e}",
                        entry.packfile_key,
                        entry.pattern,
                        path.display()
                    ))
                })?;
                slot.insert(ArcSwap::from_pointee(index));
            }
            // Sinon : packfile_key déjà ouvert pour une RouteEntry
            // précédente — pas de second open()/mmap() du même fichier.
        }

        Ok(Self { indices })
    }

    /// Lecture lock-free de l'Arc courant pour `key`. `None` si la clé n'a
    /// jamais été provisionnée à la construction — cas attendu (route
    /// malformée ou obsolète côté appelant), pas une violation d'invariant.
    pub fn load(&self, key: &str) -> Option<Arc<PackHtmlIndex>> {
        self.indices.get(key).map(|slot| slot.load_full())
    }

    /// Remplacement atomique de l'Arc courant pour `key`. Panique si `key`
    /// est absente de la topologie figée à la construction.
    pub fn store(&self, key: &str, new_index: Arc<PackHtmlIndex>) {
        match self.indices.get(key) {
            Some(slot) => slot.store(new_index),
            None => panic!(
                "LiveRegistry::store : clé \"{key}\" absente de la topologie figée \
                 à la construction — violation de l'invariant AOT (clé non \
                 provisionnée par with_indices()/cold_start())"
            ),
        }
    }
}

// =============================================================================
// Tests — Jalon 2 (concurrence ArcSwap, std::thread, sans Tokio)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_html_format::{PackfileEntry, write_packfile_footer};
    use crate::pack_html_index::ALIVE_INSTANCES;
    use std::io::{BufWriter, Write};
    use std::os::unix::fs::FileExt;
    use std::sync::atomic::{AtomicBool, Ordering};

    const KEY: &str = "jalon2_test_key";

    const IDS_AND_FRAGMENTS: &[(i64, &[u8])] = &[
        (1, b"<p>fragment-un</p>"),
        (2, b"<p>fragment-deux-un-peu-plus-long</p>"),
        (3, b"<p>f3</p>"),
        (4, b"<p>fragment-quatre-final</p>"),
    ];

    const NUM_READERS: usize = 16;
    const READS_PER_READER: usize = 5_000;
    const NUM_GENERATIONS: usize = 500;

    fn build_generation(tag: usize) -> PackHtmlIndex {
        let path = std::env::temp_dir().join(format!(
            "marius_registry_jalon2_{}_{tag}.bin",
            std::process::id()
        ));

        let mut blob = Vec::new();
        let mut entries = Vec::with_capacity(IDS_AND_FRAGMENTS.len());
        let mut offset = 0u64;
        for (id, frag) in IDS_AND_FRAGMENTS {
            blob.extend_from_slice(frag);
            entries.push(PackfileEntry {
                id: *id,
                offset,
                len: frag.len() as u32,
                _pad: [0u8; 4],
            });
            offset += frag.len() as u64;
        }

        {
            let file = std::fs::File::create(&path).expect("création fichier temporaire");
            let mut writer = BufWriter::new(file);
            writer.write_all(&blob).expect("écriture du blob");
            write_packfile_footer(&mut writer, blob.len() as u64, &entries)
                .expect("écriture footer+index");
            writer.flush().expect("flush");
        }

        let index = PackHtmlIndex::open(&path).expect("open() doit réussir");
        let _ = std::fs::remove_file(&path);

        index
    }

    #[test]
    fn concurrent_readers_never_observe_a_torn_read_during_swaps() {
        ALIVE_INSTANCES.store(0, Ordering::Relaxed);

        let initial = build_generation(0);

        let mut indices = HashMap::new();
        indices.insert(KEY, ArcSwap::from_pointee(initial));
        let registry = Arc::new(LiveRegistry::with_indices(indices));

        let mismatch = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::with_capacity(NUM_READERS + 1);

        for reader_id in 0..NUM_READERS {
            let registry = Arc::clone(&registry);
            let mismatch = Arc::clone(&mismatch);
            handles.push(std::thread::spawn(move || {
                for i in 0..READS_PER_READER {
                    let (id, expected) =
                        IDS_AND_FRAGMENTS[(reader_id + i) % IDS_AND_FRAGMENTS.len()];

                    let idx = registry.load(KEY).expect("clé provisionnée au démarrage");
                    let (offset, len) = idx.lookup(id).unwrap_or_else(|| {
                        panic!("id={id} absent — présent dans chaque génération")
                    });

                    let mut buf = vec![0u8; len as usize];
                    idx.file()
                        .read_at(&mut buf, offset)
                        .expect("read_at ne doit jamais échouer sur un fd valide");

                    if buf != expected {
                        mismatch.store(true, Ordering::Relaxed);
                    }
                }
            }));
        }

        {
            let registry = Arc::clone(&registry);
            handles.push(std::thread::spawn(move || {
                for tag in 1..=NUM_GENERATIONS {
                    let next = build_generation(tag);
                    registry.store(KEY, Arc::new(next));
                }
            }));
        }

        for h in handles {
            h.join().expect("un thread du test a paniqué");
        }

        assert!(
            !mismatch.load(Ordering::Relaxed),
            "au moins une lecture a renvoyé un fragment différent de l'attendu \
             pendant un remplacement concurrent — lecture incohérente détectée"
        );

        assert_eq!(
            ALIVE_INSTANCES.load(Ordering::Relaxed),
            1,
            "instances PackHtmlIndex encore vivantes après thread::join() de tous \
             les lecteurs et de l'écrivain — fuite détectée (une ancienne \
             génération n'a pas été libérée)"
        );
    }

    #[test]
    #[should_panic(expected = "violation de l'invariant AOT")]
    fn store_on_unprovisioned_key_panics() {
        let registry = LiveRegistry::with_indices(HashMap::new());
        let index = build_generation(9_999);
        registry.store("clé_jamais_provisionnée", Arc::new(index));
    }

    #[test]
    fn load_on_unprovisioned_key_returns_none() {
        let registry = LiveRegistry::with_indices(HashMap::new());
        assert!(registry.load("clé_jamais_provisionnée").is_none());
    }

    // =========================================================================
    // Tests — Jalon 3 (cold_start)
    //
    // packfile_path_for() est un contrat fixe ("artifacts/{key}.bin", relatif
    // au CWD du processus de test — le répertoire du crate sous `cargo test`).
    // Clés uniques par test (pid + compteur, leak 'static volontaire — usage
    // de test uniquement) pour rester indépendant des fixtures Jalon 2 et
    // d'éventuelles exécutions parallèles d'autres tests de ce module.
    // =========================================================================

    fn unique_cold_start_key(label: &str) -> &'static str {
        use std::sync::atomic::AtomicU64;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        Box::leak(format!("cold_start_{label}_{}_{n}", std::process::id()).into_boxed_str())
    }

    /// Écrit un packfile synthétique au chemin résolu par packfile_path_for —
    /// même convention que cold_start() lui-même, pour que le test prouve
    /// quelque chose sur cold_start(), pas sur un chemin arbitraire.
    fn write_packfile_at_key(key: &'static str, ids_and_fragments: &[(i64, &[u8])]) {
        let path = packfile_path_for(key);
        std::fs::create_dir_all(
            path.parent()
                .expect("packfile_path_for retourne toujours un chemin avec parent"),
        )
        .expect("création du répertoire artifacts/ de test");

        let mut blob = Vec::new();
        let mut entries = Vec::with_capacity(ids_and_fragments.len());
        let mut offset = 0u64;
        for (id, frag) in ids_and_fragments {
            blob.extend_from_slice(frag);
            entries.push(PackfileEntry {
                id: *id,
                offset,
                len: frag.len() as u32,
                _pad: [0u8; 4],
            });
            offset += frag.len() as u64;
        }

        let file = std::fs::File::create(&path).expect("création packfile de test");
        let mut writer = BufWriter::new(file);
        writer.write_all(&blob).expect("écriture blob");
        write_packfile_footer(&mut writer, blob.len() as u64, &entries)
            .expect("écriture footer+index");
        writer.flush().expect("flush");
    }

    #[test]
    fn cold_start_opens_every_route_table_packfile_and_lookup_succeeds() {
        let key_a = unique_cold_start_key("a");
        let key_b = unique_cold_start_key("b");

        write_packfile_at_key(key_a, &[(1, b"<p>a-un</p>"), (2, b"<p>a-deux</p>")]);
        write_packfile_at_key(key_b, &[(1, b"<p>b-un</p>")]);

        let route_table: &'static [RouteEntry] = Box::leak(
            vec![
                RouteEntry {
                    pattern: "/a/:id",
                    packfile_key: key_a,
                    id_source: IdSource::PathParam("id"),
                },
                RouteEntry {
                    pattern: "/b",
                    packfile_key: key_b,
                    id_source: IdSource::Fixed(1),
                },
            ]
            .into_boxed_slice(),
        );

        let registry = LiveRegistry::cold_start(route_table).expect("cold_start doit réussir");

        let idx_a = registry
            .load(key_a)
            .expect("clé a provisionnée par cold_start");
        assert_eq!(idx_a.lookup(1), Some((0, 11)));

        let idx_b = registry
            .load(key_b)
            .expect("clé b provisionnée par cold_start");
        assert_eq!(idx_b.lookup(1), Some((0, 11)));
    }

    #[test]
    fn cold_start_fails_fast_on_missing_packfile_not_panics() {
        let missing_key = unique_cold_start_key("missing");

        let route_table: &'static [RouteEntry] = Box::leak(
            vec![RouteEntry {
                pattern: "/absent/:id",
                packfile_key: missing_key,
                id_source: IdSource::PathParam("id"),
            }]
            .into_boxed_slice(),
        );

        // Aucun fichier écrit pour missing_key — cold_start doit échouer
        // (Err), jamais paniquer : un packfile manquant au démarrage est une
        // erreur fatale de déploiement, mais propre (?), pas un panic non
        // structuré.
        let result = LiveRegistry::cold_start(route_table);
        assert!(
            result.is_err(),
            "cold_start doit échouer proprement quand un packfile référencé par \
             ROUTE_TABLE est introuvable"
        );
    }

    #[test]
    fn cold_start_deduplicates_shared_packfile_key_across_routes() {
        // Même contrainte opérationnelle que le test Jalon 2 : ALIVE_INSTANCES
        // est partagé par tout le binaire de test du crate — exécuter
        // isolément ou avec --test-threads=1 pour une assertion fiable.
        ALIVE_INSTANCES.store(0, Ordering::Relaxed);

        let shared_key = unique_cold_start_key("shared");
        write_packfile_at_key(shared_key, &[(1, b"<p>partage</p>")]);

        // Deux routes distinctes (page complète + fragment HTMX, spec §4)
        // pointant vers le même packfile_key.
        let route_table: &'static [RouteEntry] = Box::leak(
            vec![
                RouteEntry {
                    pattern: "/page/:id",
                    packfile_key: shared_key,
                    id_source: IdSource::PathParam("id"),
                },
                RouteEntry {
                    pattern: "/fragment/:id",
                    packfile_key: shared_key,
                    id_source: IdSource::PathParam("id"),
                },
            ]
            .into_boxed_slice(),
        );

        let _registry = LiveRegistry::cold_start(route_table).expect("cold_start doit réussir");

        assert_eq!(
            ALIVE_INSTANCES.load(Ordering::Relaxed),
            1,
            "deux RouteEntry partageant packfile_key doivent produire un seul \
             PackHtmlIndex — pas un open()/mmap() par route"
        );
    }
}
