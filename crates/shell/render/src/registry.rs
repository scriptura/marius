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
// concurrent sûr : aucune requête ne mute jamais la topologie de routage,
// seulement les pointeurs qu'elle porte. D'où le champ `indices` privé et
// les méthodes load()/store() enveloppantes ci-dessous (arbitrage explicite,
// remplace l'accès direct au champ visible dans les extraits §6.1/§7 de la
// spec — ces extraits décrivent un appel depuis Phase 3/4, hors périmètre
// ici ; l'encapsulation est le choix retenu pour cette session).
//
// cold_start() (spec §5, itère sur ROUTE_TABLE) est explicitement REPORTÉ à
// la Phase 3 : ROUTE_TABLE n'existe pas dans le projet avant cette phase
// (roadmap — "écrite à la main", Phase 3). L'implémenter ici créerait une
// dépendance vers un artefact absent. with_indices() est le seul
// constructeur de cette session — point d'injection pour le pilote de test
// ci-dessous, et plus tard pour cold_start() lui-même.
// =============================================================================

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::pack_html_index::PackHtmlIndex;

/// Registre vivant des index de packfiles HTML — un `ArcSwap<PackHtmlIndex>`
/// par `packfile_key`, échangeable sans verrou exclusif.
pub struct LiveRegistry {
    /// Topologie figée après construction. Privé : tout accès passe par
    /// load()/store() ci-dessous, pour que la distinction clé-absente
    /// (lecture : cas attendu, route malformée) / clé-absente (écriture :
    /// violation d'invariant AOT) reste portée par le type, pas par
    /// convention chez l'appelant.
    indices: HashMap<&'static str, ArcSwap<PackHtmlIndex>>,
}

impl LiveRegistry {
    /// Construit le registre à partir d'une table déjà peuplée.
    ///
    /// Remplace cold_start() pour cette session — cf. note de module
    /// ci-dessus sur la dépendance ROUTE_TABLE reportée à la Phase 3. C'est
    /// ce constructeur que le pilote de test multithread (Jalon 2, plus bas)
    /// utilise pour peupler le registre avant de lancer lecteurs et
    /// écrivain.
    pub fn with_indices(indices: HashMap<&'static str, ArcSwap<PackHtmlIndex>>) -> Self {
        Self { indices }
    }

    /// Lecture lock-free de l'Arc courant pour `key`. `None` si la clé n'a
    /// jamais été provisionnée à la construction — cas attendu (route
    /// malformée ou obsolète côté appelant), pas une violation d'invariant :
    /// à la différence de store() ci-dessous, une lecture sur une clé
    /// absente n'indique aucun bug du Render Shell lui-même.
    ///
    /// `load_full()`, pas `load()` : retourne un `Arc<PackHtmlIndex>`
    /// possédé, sans lier sa durée de vie à `&self` — l'appelant peut
    /// changer de thread (`spawn_blocking`, Phase 3) en conservant l'Arc,
    /// sans porter d'emprunt sur le registre pendant ce temps.
    pub fn load(&self, key: &str) -> Option<Arc<PackHtmlIndex>> {
        self.indices.get(key).map(|slot| slot.load_full())
    }

    /// Remplacement atomique de l'Arc courant pour `key`.
    ///
    /// Panique si `key` est absente de la topologie figée à la
    /// construction. Décision explicite (l'alternative — ignorer
    /// silencieusement — a été écartée) : une clé non provisionnée par
    /// with_indices()/cold_start() au démarrage signale que l'appelant (le
    /// futur Dispatcher, Phase 4) tente de régénérer un packfile que la
    /// topologie n'a jamais déclaré — un bug d'écriture, pas une donnée
    /// absente à tolérer. Ignorer silencieusement masquerait ce bug derrière
    /// un no-op indiscernable d'un store() réussi. Même discipline que
    /// cold_start() (spec §5) face à un packfile manquant au démarrage :
    /// échec immédiat et bruyant, pas une dégradation silencieuse.
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
// Tests — Jalon 2
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_html_format::{write_packfile_footer, PackfileEntry};
    use crate::pack_html_index::ALIVE_INSTANCES;
    use std::io::{BufWriter, Write};
    use std::os::unix::fs::FileExt;
    use std::sync::atomic::{AtomicBool, Ordering};

    const KEY: &str = "jalon2_test_key";

    /// Jeu d'ids/fragments fixe, identique dans CHAQUE génération produite
    /// par build_generation() — seule l'identité de l'instance PackHtmlIndex
    /// qui sert un id change entre deux store(), jamais le contenu attendu
    /// pour cet id. Choix délibéré, pas un raccourci : faire varier le
    /// contenu par génération ajouterait une seconde variable ("quelle
    /// génération suis-je censé observer ?") que ArcSwap ne garantit
    /// justement pas de prédire côté lecteur — ça diluerait la propriété
    /// réellement testée (cohérence d'une lecture pendant un remplacement
    /// concurrent) derrière une question sans réponse définie.
    const IDS_AND_FRAGMENTS: &[(i64, &[u8])] = &[
        (1, b"<p>fragment-un</p>"),
        (2, b"<p>fragment-deux-un-peu-plus-long</p>"),
        (3, b"<p>f3</p>"),
        (4, b"<p>fragment-quatre-final</p>"),
    ];

    const NUM_READERS: usize = 16;
    const READS_PER_READER: usize = 5_000;
    const NUM_GENERATIONS: usize = 500;

    /// Construit une génération de packfile synthétique sur disque (mêmes
    /// ids/fragments à chaque appel — voir IDS_AND_FRAGMENTS ci-dessus),
    /// l'ouvre via PackHtmlIndex::open(), puis retire le fichier du disque.
    ///
    /// Le retrait immédiat est volontaire, pas un oubli de nettoyage tardif :
    /// le fd et le mmap déjà établis dans `PackHtmlIndex` restent valides
    /// après unlink (sémantique POSIX — l'inode ne disparaît qu'au dernier
    /// close()), donc aucune accumulation de fichiers temporaires sur
    /// NUM_GENERATIONS itérations. Exerce directement le point de vigilance
    /// n°3 de la roadmap (fuite fd/inode), déplacé en Phase 2.
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

        // Best-effort : index a déjà capturé tout ce qu'il lui faut (fd +
        // mmap). Un échec de remove_file ici ne remettrait rien en cause.
        let _ = std::fs::remove_file(&path);

        index
    }

    /// N lecteurs natifs (std::thread) en boucle serrée load()→lookup()→
    /// read_at()→comparaison stricte, concurrents à 1 écrivain qui store()
    /// en boucle. Assertion finale double : zéro lecture incohérente, ET
    /// ALIVE_INSTANCES == 1 après jointure de tous les threads.
    ///
    /// Portée exacte de cette seconde assertion (cf. handoff, ne pas la
    /// confondre avec "store()+1") : ALIVE_INSTANCES == 1 prouve qu'aucune
    /// ancienne génération n'a fui — pas seulement qu'on a bien construit le
    /// nombre attendu d'instances (ce chiffre est trivialement déjà connu du
    /// pilote lui-même, qui contrôle ses propres appels à open()).
    ///
    /// Contrainte opérationnelle héritée du compteur global #[cfg(test)] :
    /// ALIVE_INSTANCES est partagé par tout le binaire de test du crate, pas
    /// seulement par ce test. Si les tests de pack_html_index.rs (Jalon 1)
    /// tournent EN PARALLÈLE de celui-ci (comportement par défaut de
    /// `cargo test`), leurs propres open()/drop() polluent le compteur
    /// pendant la fenêtre d'assertion. Exécuter isolément :
    ///   cargo test -p marius-render --lib \
    ///     concurrent_readers_never_observe_a_torn_read_during_swaps
    /// ou la suite complète avec `-- --test-threads=1`. Le std::sync::atomic
    /// ::Ordering::Relaxed retenu ici suffit (pas de donnée associée à
    /// synchroniser via ce compteur, seul son décompte importe) — mais ne
    /// protège pas contre l'exécution concurrente d'autres tests, par
    /// construction : c'est une contrainte de lancement, pas un bug
    /// d'implémentation à corriger silencieusement ici.
    #[test]
    fn concurrent_readers_never_observe_a_torn_read_during_swaps() {
        // Remet le compteur à zéro pour ce test précis : neutralise le
        // résidu d'un test précédent exécuté en série dans le même process
        // (n'isole pas d'une exécution réellement concurrente — voir
        // contrainte opérationnelle ci-dessus).
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
                    let (offset, len) = idx
                        .lookup(id)
                        .unwrap_or_else(|| panic!("id={id} absent — présent dans chaque génération"));

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

    /// store() sur une clé jamais provisionnée doit paniquer — pas un no-op
    /// silencieux. Couvre explicitement la branche `None` du match de
    /// store(), sans dépendre du test multithread ci-dessus pour l'exercer.
    #[test]
    #[should_panic(expected = "violation de l'invariant AOT")]
    fn store_on_unprovisioned_key_panics() {
        let registry = LiveRegistry::with_indices(HashMap::new());
        let index = build_generation(9_999);
        registry.store("clé_jamais_provisionnée", Arc::new(index));
    }

    /// load() sur une clé jamais provisionnée retourne None — pas de panic.
    /// Distinction délibérée avec store() : une route malformée en lecture
    /// n'est pas un bug du Render Shell.
    #[test]
    fn load_on_unprovisioned_key_returns_none() {
        let registry = LiveRegistry::with_indices(HashMap::new());
        assert!(registry.load("clé_jamais_provisionnée").is_none());
    }
}
