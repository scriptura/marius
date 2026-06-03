// marius-render · dispatcher.rs
// Dispatcher (Reactive Orchestrator) — Shell uniquement.
// Dépend de Tokio, Rayon, SQLx : ne peut pas être dans le Core.
//
// Migration depuis marius-collector (Phase 1 refactoring) :
// Le Collector<MAX, WORDS> reste dans le Core (marius-collector, zéro dépendance).
// Le Dispatcher vit ici, dans le Shell, car il orchestre les I/O.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use tokio::sync::Notify;
use tokio::time::interval;

use marius_collector::Collector;
use marius_projection::Projection;

pub struct DispatcherConfig {
    pub tick_default:    Duration,
    pub tick_min:        Duration,
    pub tick_max:        Duration,
    /// Seuil volumétrique — main.rs appelle notify si insert() retourne ThresholdReached.
    pub threshold_flush: usize,
    pub threshold_low:   usize,
    pub threshold_high:  usize,
    /// Budget de rendu au-delà duquel on passe en tick_max.
    pub render_budget:   Duration,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            tick_default:    Duration::from_millis(500),
            tick_min:        Duration::from_millis(100),
            tick_max:        Duration::from_millis(2_000),
            threshold_flush: 128,
            threshold_low:   10,
            threshold_high:  100,
            render_budget:   Duration::from_millis(200),
        }
    }
}

pub struct Dispatcher<P: Projection, const MAX: usize, const WORDS: usize> {
    collector: &'static Collector<MAX, WORDS>,
    notify:    Arc<Notify>,
    pool:      sqlx::PgPool,
    config:    DispatcherConfig,
    _phantom:  std::marker::PhantomData<P>,
}

impl<P: Projection, const MAX: usize, const WORDS: usize> Dispatcher<P, MAX, WORDS> {
    pub fn new(
        collector: &'static Collector<MAX, WORDS>,
        notify:    Arc<Notify>,
        pool:      sqlx::PgPool,
        config:    DispatcherConfig,
    ) -> Self {
        Self { collector, notify, pool, config, _phantom: std::marker::PhantomData }
    }

    pub async fn run(self) {
        let mut current_tick = self.config.tick_default;
        let mut ticker       = interval(current_tick);

        loop {
            tokio::select! {
                _ = ticker.tick()          => {}
                _ = self.notify.notified() => {}
            }

            let ids = self.collector.flush();
            if ids.is_empty() { continue; }

            let t0      = Instant::now();
            let records = match P::fetch_batch(&self.pool, &ids).await {
                Ok(r)  => r,
                Err(e) => { eprintln!("[dispatcher] fetch_batch: {e}"); continue; }
            };

            // Projection parallèle.
            // into_par_iter() : T: Send suffisant (pas T: Sync).
            //
            // fetch_batch retourne Vec<(Record, VarlenOwned)> (ADR-003).
            // Déstructuration du tuple : record et varlena sont Send + 'static,
            // traversent la frontière Rayon sans contrainte de lifetime.
            // render() reconstruit les &str localement via as_deref() — zéro copie,
            // durée de vie confinée à la closure.
            //
            // Buffer réutilisé par thread Rayon via map_with().
            // map_with() clone le buffer seed (capacité 0) une fois par thread
            // au démarrage du pool, puis le passe par &mut à chaque itération.
            // buf.clear() remet len=0 sans libérer la mémoire allouée :
            // après le premier render(), le buffer est à la bonne capacité
            // et les itérations suivantes n'allouent plus.
            // Invariant : une seule allocation par thread Rayon par tick de Dispatcher,
            // indépendamment du nombre d'enregistrements dans le batch.
            records.into_par_iter().map_with(String::new(), |buf, (record, varlena)| {
                buf.clear();
                P::render(&record, &varlena, buf);

                let path = P::artifact_path(&record);
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&path, buf.as_bytes()) {
                    eprintln!("[dispatcher] write {:?}: {e}", path);
                }
            }).for_each(|_| {});

            let new_tick = self.adapt_tick(ids.len(), t0.elapsed());
            if new_tick != current_tick {
                current_tick = new_tick;
                // interval() Tokio ne supporte pas le changement de période à chaud.
                ticker = interval(current_tick);
            }
        }
    }

    fn adapt_tick(&self, batch_size: usize, elapsed: Duration) -> Duration {
        let pressure = elapsed    > self.config.render_budget
                    || batch_size > self.config.threshold_high;
        let quiet    = batch_size < self.config.threshold_low;
        match (pressure, quiet) {
            (true, _) => self.config.tick_max,
            (_, true) => self.config.tick_min,
            _         => self.config.tick_default,
        }
    }
}

#[cfg(test)]
mod tests {
    use rayon::prelude::*;
    use marius_schema::{
        ContentCoreStorageRow,
        ContentCoreVarlenOwned,
        ContentCoreProjection,
        CONTENT_CORE_TOTAL_CAP,
    };
    // Trait requis en scope pour résoudre ContentCoreProjection::render()
    // et ContentCoreProjection::artifact_path() sous forme qualifiée.
    // use as _ ne suffit pas pour les appels Type::method() — seul
    // le trait nommé permet la résolution statique de la méthode de trait.
    #[allow(unused_imports)]
    use marius_projection::Projection;

    // =========================================================================
    // Constantes du jeu de données
    // =========================================================================

    /// Taille du lot : suffisant pour que Rayon subdivise le travail sur
    /// plusieurs threads et que chaque thread traite plusieurs enregistrements,
    /// rendant observable la stabilité inter-itération de la capacité.
    const BATCH_SIZE: usize = 1_000;

    /// Chaîne varlena agressive : contient les cinq caractères dangereux en HTML
    /// ('&', '<', '>', '"', '\'') dans un ordre qui exercice marius_html_escape()
    /// dans toutes ses branches.
    /// Longueur : 43 chars → après escape : jusqu'à 43 × 5 = 215 octets.
    /// La chaîne est répétée pour saturer max_escaped_len sans le dépasser.
    const AGGRESSIVE_VARLENA: &str = r#"<html> & "Marius" & 'Engine'</html>"#;

    // =========================================================================
    // Helpers de construction du jeu de données
    // =========================================================================

    /// Instancie un StorageRow avec les valeurs pires cas des types fixed-length.
    /// Ces valeurs maximisent DYNAMIC_CAP côté fixed, garantissant que buf est
    /// pré-alloué à sa borne supérieure exacte avant le premier render().
    fn worst_case_storage() -> ContentCoreStorageRow {
        ContentCoreStorageRow {
            published_at:        i64::MIN,  // 20 chars — max I64
            created_at:          i64::MIN,
            modified_at:         i64::MIN,
            document_id:         i32::MIN,  // 11 chars — max I32
            author_entity_id:    i32::MIN,
            status:              i16::MIN,  // 6 chars  — max I16
            is_readable:         false,     // 5 chars  — "false" > "true"
            is_commentable:      false,
            is_visible_comments: false,
        }
    }

    /// Instancie un VarlenOwned avec des chaînes agressives dans tous les champs
    /// varlena documentés. Les champs non listés reçoivent None via Default.
    ///
    /// L'utilisation de AGGRESSIVE_VARLENA dans headline, description et
    /// alternative_headline couvre les trois branches de marius_html_escape()
    /// les plus fréquentes ('&', '<', '"') dans un seul appel render().
    fn aggressive_varlena() -> ContentCoreVarlenOwned {
        ContentCoreVarlenOwned {
            headline:             Some(AGGRESSIVE_VARLENA.to_string()),
            description:          Some(AGGRESSIVE_VARLENA.repeat(3)),
            alternative_headline: Some(AGGRESSIVE_VARLENA.repeat(2)),
            ..Default::default()
        }
    }

    // =========================================================================
    // test_hot_path_pipeline_stress
    // =========================================================================

    /// Test d'intégration déterministe du chemin critique de rendu parallèle.
    ///
    /// ─── Objectifs ───────────────────────────────────────────────────────────
    ///
    ///   1. Correction fonctionnelle : les caractères HTML dangereux sont
    ///      échappés dans chaque fragment produit.
    ///
    ///   2. No-realloc inter-itération (invariant primaire) :
    ///      La capacité du buffer de chaque thread est capturée après son premier
    ///      render(). Elle doit rester identique pour toutes les itérations
    ///      suivantes du même thread. Une croissance signale que DYNAMIC_CAP
    ///      sous-estime le pire cas pour au moins un champ.
    ///
    ///   3. Débit sans panique : 1000 records projetés sans erreur.
    ///
    /// ─── Mécanique du seed (String, usize) ───────────────────────────────────
    ///
    ///   Le seed de map_with est (String::new(), 0usize).
    ///   - String::new() : buffer réutilisé par thread (no-alloc après premier rendu).
    ///   - 0usize        : capacité de référence. 0 = "pas encore observée".
    ///
    ///   Au premier rendu du thread (ref_cap == 0) :
    ///     1. render() est appelé → buf atteint TOTAL_CAP.
    ///     2. ref_cap est fixé à buf.capacity().
    ///     3. La correction HTML est vérifiée sur ce premier fragment.
    ///
    ///   Aux rendus suivants (ref_cap > 0) :
    ///     1. buf.clear() remet len=0, capacity inchangée.
    ///     2. render() est appelé.
    ///     3. Assertion : buf.capacity() == ref_cap.
    ///        Un écart prouve un realloc inter-itération.
    ///
    /// ─── Pourquoi map_with retourne () ───────────────────────────────────────
    ///
    ///   La closure retourne () — les assertions sont posées inline, pas
    ///   collectées. map_with est consommé par for_each(|_| {}).
    ///   Rayon garantit que la closure est appelée séquentiellement par thread
    ///   sur les éléments qui lui sont assignés : ref_cap est donc un état
    ///   mono-thread sans besoin de synchronisation.
    #[test]
    fn test_hot_path_pipeline_stress() {
        // Construction du lot : 1000 tuples (StorageRow, VarlenOwned).
        // Tous identiques : on teste la stabilité de la capacité, pas la variété
        // des données. Les pires cas fixes + varlena agressif maximisent buf.len().
        let batch: Vec<(ContentCoreStorageRow, ContentCoreVarlenOwned)> = (0..BATCH_SIZE)
            .map(|_| (worst_case_storage(), aggressive_varlena()))
            .collect();

        // Compteur de records effectivement projetés, pour vérifier qu'aucun
        // n'est silencieusement sauté par Rayon.
        // AtomicUsize : seul état partagé entre threads, lecture finale hors Rayon.
        let projected = std::sync::atomic::AtomicUsize::new(0);

        // Pattern map_with : seed (buf, ref_cap) cloné une fois par thread Rayon.
        // ref_cap = 0 encode "pas encore observé" — 0 n'est jamais une capacité
        // valide après un premier render() (TOTAL_CAP > 0 par construction).
        batch
            .into_par_iter()
            .map_with(
                (String::new(), 0usize), // seed : (buffer réutilisé, ref_cap)
                |(buf, ref_cap), (storage, varlena)| {
                    if *ref_cap == 0 {
                        // ── Premier rendu de ce thread ────────────────────────
                        // buf part de capacité 0. render() appelle
                        // buf.reserve(STATIC_CAP + DYNAMIC_CAP) en premier.
                        // Après render(), buf.capacity() == TOTAL_CAP (ou plus,
                        // selon l'allocateur système qui peut arrondir à la page).
                        ContentCoreProjection::render(&storage, &varlena, buf);

                        // Fixe la référence pour toutes les itérations suivantes.
                        *ref_cap = buf.capacity();

                        // Borne inférieure : le buffer a au moins été alloué à TOTAL_CAP.
                        // L'allocateur peut avoir arrondi à la page supérieure,
                        // donc on n'assert pas l'égalité exacte ici.
                        assert!(
                            *ref_cap >= CONTENT_CORE_TOTAL_CAP,
                            "Premier render() : capacité {ref_cap} < TOTAL_CAP {}. \
                             Fragment-Forge sous-estime STATIC_CAP + DYNAMIC_CAP.",
                            CONTENT_CORE_TOTAL_CAP
                        );

                        // ── Correction HTML : vérification sur le premier fragment ─
                        // Les cinq entités HTML doivent apparaître au moins une fois.
                        // Si marius_html_escape() manque une branche, le caractère
                        // brut apparaît dans le fragment — XSS potentiel.
                        assert!(
                            buf.contains("&amp;"),
                            "Escape manquant : '&' non transformé en '&amp;'"
                        );
                        assert!(
                            buf.contains("&lt;"),
                            "Escape manquant : '<' non transformé en '&lt;'"
                        );
                        assert!(
                            buf.contains("&gt;"),
                            "Escape manquant : '>' non transformé en '&gt;'"
                        );
                        assert!(
                            buf.contains("&quot;"),
                            "Escape manquant : '\"' non transformé en '&quot;'"
                        );
                        assert!(
                            buf.contains("&#39;"),
                            "Escape manquant : '\\'' non transformé en '&#39;'"
                        );

                        // Structure HTML minimale.
                        assert!(buf.starts_with("<article"), "tag ouvrant absent");
                        assert!(buf.ends_with("</article>"), "tag fermant absent");

                    } else {
                        // ── Rendus suivants : invariant no-realloc inter-itération ─
                        // buf.clear() remet len=0, capacity inchangée.
                        // render() ne doit pas dépasser la capacité établie.
                        buf.clear();
                        let cap_before = buf.capacity();

                        ContentCoreProjection::render(&storage, &varlena, buf);

                        // Assertion primaire : la capacité ne doit pas avoir crû.
                        // Une croissance = realloc = violation de l'invariant no-realloc.
                        // L'allocateur ne réduit jamais la capacité spontanément,
                        // donc cap_before == *ref_cap est garanti si aucun realloc.
                        assert_eq!(
                            buf.capacity(), cap_before,
                            "REALLOC inter-itération détecté sur thread Rayon : \
                             capacité {} → {} après render(). \
                             DYNAMIC_CAP ({}) sous-estime le pire cas varlena.",
                            cap_before, buf.capacity(), CONTENT_CORE_TOTAL_CAP
                        );
                    }

                    projected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                },
            )
            .for_each(|_| {});

        // ── Vérification de débit ─────────────────────────────────────────────
        // Tous les enregistrements du lot ont été projetés.
        // Un écart signalerait un bug dans la distribution Rayon ou une panique
        // silencieuse (impossible en Rust safe, mais détecté par précaution).
        let total = projected.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            total, BATCH_SIZE,
            "Débit incomplet : {total}/{BATCH_SIZE} records projetés. \
             Vérifier la distribution Rayon et l'absence de short-circuit."
        );

        println!(
            "[stress] {BATCH_SIZE} records projetés — no-realloc inter-itération vérifié. \
             TOTAL_CAP = {CONTENT_CORE_TOTAL_CAP}B."
        );
    }
}
