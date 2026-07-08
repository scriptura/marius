// marius-render · dispatcher.rs
// Dispatcher (Reactive Orchestrator) — Shell uniquement.
// Dépend de Tokio, SQLx : ne peut pas être dans le Core.
//
// Migration depuis marius-collector (Phase 1 refactoring) :
// Le Collector<MAX, WORDS> reste dans le Core (marius-collector, zéro dépendance).
// Le Dispatcher vit ici, dans le Shell, car il orchestre les I/O.
//
// ─── Écriture (Phase 4, état résolu) ──────────────────────────────────────────
//
//   run() ne fait jamais d'écriture directe : chaque tick délègue à
//   regenerate_and_swap (regenerate.rs), qui réalise fetch_batch par chunk,
//   merge_sweep, écriture .tmp + fsync + rename atomique, puis
//   LiveRegistry::store(). Compatible par construction avec le modèle
//   mmap/ArcSwap de registry.rs (Phase 2) — aucune écriture en place,
//   aucun footer manquant.
//
//   Deux champs portent ce câblage : `registry` (Arc partagé avec la
//   frontière Axum de lecture, main.rs — cloné avant le tokio::spawn de ce
//   Dispatcher, jamais reconstruit) et `packfile_key` (clé
//   LiveRegistry/packfile_path_for ciblée par CE Dispatcher — doit
//   correspondre exactement à la RouteEntry qui sert ces mêmes données en
//   lecture ; pas dérivée de P::packfile_path(), qui n'est pas ce contrat
//   d'adressage).
//
//   Sémantique de Collector::flush() : résolue et testée depuis
//   regenerate.rs (untouched_entities_survive_successive_incremental_merges_then_delete).
//   flush() retourne l'ensemble complet des ids de la cible, pas un delta —
//   regenerate_and_swap réécrit donc le packfile en entier à chaque appel,
//   par construction, et non par fusion incrémentale.
//
//   ids.sort_unstable() avant l'appel : précondition du format on-disk
//   (spec §3, "index trié par id ASC", non vérifiée par le format
//   lui-même) — défense explicite, pas une supposition héritée de l'ordre
//   d'itération interne de Collector::flush().
//
// ─── Séparation async / sync (inchangée) ──────────────────────────────────────
//
//   run()              : boucle asynchrone Tokio. Gère le tick adaptatif,
//                         appelle regenerate_and_swap().
//   render_batch_pure() : logique de rendu séquentielle pure, synchrone,
//                         sans I/O disque — utilisée par les benchmarks
//                         Divan. Non concernée par le bug Append (jamais
//                         touchait le filesystem). Réalignée cette session :
//                         portait un pipeline Rayon résiduel (map_with),
//                         désormais buffer unique réutilisé, conforme au
//                         manifeste révisé (28 juin 2026).

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{LiveRegistry, regenerate_and_swap};

use tokio::sync::Notify;
use tokio::time::interval;

use marius_collector::Collector;
use marius_projection::Projection;

/// Correction Phase 5.1 (impasse mécanique signalée, pas une modification de
/// contrat) : `Clone, Copy` ajoutés ici — absents jusqu'à présent. Sans cela,
/// `SHARDS[i].config` (table de configuration unifiée, main.rs) ne peut pas
/// être lu par valeur : un champ derrière une référence `'static` (`SHARDS:
/// &'static [ShardMetadata]`) ne peut pas être déplacé hors de son
/// emplacement sans `Copy`. Tous les champs sont déjà `Copy` (`Duration`,
/// `usize`) — la dérivation est donc gratuite (aucune allocation, aucune
/// indirection ajoutée) et n'altère aucune sémantique existante.
#[derive(Clone, Copy)]
pub struct DispatcherConfig {
    pub tick_default: Duration,
    pub tick_min: Duration,
    pub tick_max: Duration,
    /// Seuil volumétrique — main.rs appelle notify si insert() retourne ThresholdReached.
    pub threshold_flush: usize,
    pub threshold_low: usize,
    pub threshold_high: usize,
    /// Budget de rendu au-delà duquel on passe en tick_max.
    pub render_budget: Duration,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            tick_default: Duration::from_millis(500),
            tick_min: Duration::from_millis(100),
            tick_max: Duration::from_millis(2_000),
            threshold_flush: 128,
            threshold_low: 10,
            threshold_high: 100,
            render_budget: Duration::from_millis(200),
        }
    }
}

pub struct Dispatcher<P: Projection, const MAX: usize, const WORDS: usize> {
    collector: &'static Collector<MAX, WORDS>,
    notify: Arc<Notify>,
    pool: sqlx::PgPool,
    config: DispatcherConfig,
    total_cap: usize, // Contrat de capacité pour le BatchRenderer interne à regenerate_and_swap.
    /// Arc partagé avec la frontière Axum de lecture (main.rs) — cloné une
    /// fois avant le tokio::spawn de ce Dispatcher, jamais reconstruit ici.
    /// Phase 4 : seul moyen pour ce Dispatcher d'atteindre
    /// LiveRegistry::store() sans dépendance inverse vers marius-server.
    registry: Arc<LiveRegistry>,
    /// Clé LiveRegistry/packfile_path_for ciblée par ce Dispatcher — doit
    /// correspondre exactement au packfile_key de la/des RouteEntry qui
    /// servent ces mêmes données en lecture (ROUTE_TABLE, main.rs). Pas
    /// dérivée de P::packfile_path() — voir note de module.
    packfile_key: &'static str,
    _phantom: std::marker::PhantomData<P>,
    /// Singleton partagé entre TOUTES les instances de Dispatcher (tous
    /// packfile_key confondus) — créé une seule fois en amont (main.rs),
    /// cloné (jamais reconstruit) à chaque `Dispatcher::new()`. Régule le
    /// risque de dirty-page storm, qui est inter-shard : un seul Dispatcher
    /// n'a aucun parallélisme interne, mais N shards en tick simultané
    /// saturent le même disque. Phase 4.3.
    io_semaphore: Arc<tokio::sync::Semaphore>,
}

impl<P: Projection, const MAX: usize, const WORDS: usize> Dispatcher<P, MAX, WORDS> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        collector: &'static Collector<MAX, WORDS>,
        notify: Arc<Notify>,
        pool: sqlx::PgPool,
        config: DispatcherConfig,
        total_cap: usize,
        registry: Arc<LiveRegistry>,
        packfile_key: &'static str,
        io_semaphore: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            collector,
            notify,
            pool,
            config,
            total_cap,
            registry,
            packfile_key,
            _phantom: std::marker::PhantomData,
            io_semaphore,
        }
    }

    pub async fn run(self) {
        // Phase 5.3 — point d'injection de test, lu une seule fois avant la
        // boucle (zéro coût par tick). Absent en exploitation réelle (variable
        // jamais positionnée) → comparaison toujours fausse, aucun overhead,
        // aucun changement de comportement hors test.
        let panic_on_first_tick = std::env::var("MARIUS_DEBUG_PANIC_SHARD")
            .map(|target| target == self.packfile_key)
            .unwrap_or(false);

        let mut current_tick = self.config.tick_default;
        let mut ticker = interval(current_tick);

        loop {
            tokio::select! {
                _ = ticker.tick()          => {}
                _ = self.notify.notified() => {}
            }

            if panic_on_first_tick {
                panic!(
                    "[dispatcher] panic injecté par test (MARIUS_DEBUG_PANIC_SHARD=\"{}\")",
                    self.packfile_key
                );
            }

            let mut ids = self.collector.flush();
            if ids.is_empty() {
                continue;
            }

            // Précondition du format on-disk (spec §3) — voir note de
            // module : défense explicite, pas une supposition sur l'ordre
            // d'itération de Collector::flush().
            ids.sort_unstable();

            let t0 = Instant::now();

            if let Err(e) = regenerate_and_swap::<P>(
                &self.pool,
                &ids,
                self.total_cap,
                self.packfile_key,
                &self.registry,
                &self.io_semaphore,
            )
            .await
            {
                eprintln!(
                    "[dispatcher] regenerate_and_swap (\"{}\"): {e}",
                    self.packfile_key
                );
                continue;
            }

            let new_tick = self.adapt_tick(ids.len(), t0.elapsed());
            if new_tick != current_tick {
                current_tick = new_tick;
                ticker = interval(current_tick);
            }
        }
    }

    fn adapt_tick(&self, batch_size: usize, elapsed: Duration) -> Duration {
        let pressure =
            elapsed > self.config.render_budget || batch_size > self.config.threshold_high;
        let quiet = batch_size < self.config.threshold_low;
        match (pressure, quiet) {
            (true, _) => self.config.tick_max,
            (_, true) => self.config.tick_min,
            _ => self.config.tick_default,
        }
    }
}

// =============================================================================
// Rendu séquentiel pur — sans I/O disque
// =============================================================================

/// Rendu séquentiel pur — sans I/O disque.
///
/// ─── Rôle ────────────────────────────────────────────────────────────────────
///
///   Isole le hot path CPU (marius_html_escape + push_str + write_fmt) des
///   syscalls write(2) et create_dir_all(). Utilisée par les benchmarks Divan
///   pour mesurer le coût réel du rendu sans le bruit des I/O.
///
///   Distincte de regenerate_and_swap (Phase 4, regenerate.rs) qui, lui,
///   produit l'artefact on-disk complet (footer, index, rename atomique) —
///   render_batch_pure() ne touche jamais le filesystem, par construction.
///
/// ─── Invariants identiques à BatchRenderer::render_batch ────────────────────
///
///   Buffer unique alloué une fois, réutilisé sur tout le lot. buf.clear()
///   préserve la capacité entre enregistrements : zéro allocation intra-lot
///   après le premier render(). Concurrence inter-shard assurée par Tokio en
///   amont (Dispatcher::run), pas par ce chemin — séquentialité délibérée
///   intra-lot, conforme au manifeste révisé.
pub fn render_batch_pure<P: Projection>(batch: Vec<(P::Record, P::VarlenOwned)>) {
    let mut buf = String::new();
    for (record, varlena) in &batch {
        buf.clear();
        P::render(record, varlena, &mut buf);
    }
}

#[cfg(test)]
mod tests {
    use marius_schema::{
        CONTENT_CORE_TOTAL_CAP, ContentCoreProjection, ContentCoreStorageRow,
        ContentCoreVarlenOwned,
    };
    // Trait requis en scope pour résoudre ContentCoreProjection::render()
    // et ContentCoreProjection::packfile_path() sous forme qualifiée.
    // use as _ ne suffit pas pour les appels Type::method() — seul
    // le trait nommé permet la résolution statique de la méthode de trait.
    #[allow(unused_imports)]
    use marius_projection::Projection;

    // =========================================================================
    // Constantes du jeu de données
    // =========================================================================

    /// Taille du lot : suffisant pour rendre observable la stabilité de
    /// capacité sur de nombreuses itérations successives du buffer unique.
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
            published_at: i64::MIN, // 20 chars — max I64
            created_at: i64::MIN,
            modified_at: i64::MIN,
            document_id: i32::MIN, // 11 chars — max I32
            author_entity_id: i32::MIN,
            status: i16::MIN, // 6 chars  — max I16
            is_readable: 0,
            is_commentable: 0,
            is_visible_comments: 0,
            _pad: [0; 3], // padding pour alignement 8B
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
            headline: Some(AGGRESSIVE_VARLENA.to_string()),
            description: Some(AGGRESSIVE_VARLENA.repeat(3)),
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
    /// ─── Mécanique du buffer unique ──────────────────────────────────────────
    ///
    ///   buf est alloué une seule fois, avant la boucle, à TOTAL_CAP.
    ///   Séquentialité délibérée intra-lot : un seul buffer, réutilisé pour
    ///   chaque enregistrement — aucun état par thread à tracer.
    ///
    ///   À chaque itération :
    ///     1. cap_before capture la capacité avant le render() courant.
    ///     2. buf.clear() remet len=0, capacity inchangée.
    ///     3. render() est appelé.
    ///     4. Assertion : buf.capacity() == cap_before.
    ///        Un écart prouve une réallocation à cette itération précise.
    ///
    ///   La correction HTML (échappement, structure) est vérifiée sur le
    ///   premier fragment produit — elle ne dépend pas de l'itération, donc
    ///   une seule vérification suffit à couvrir marius_html_escape().
    #[test]
    fn test_hot_path_pipeline_stress() {
        // Construction du lot : 1000 tuples (StorageRow, VarlenOwned).
        // Tous identiques : on teste la stabilité de la capacité, pas la variété
        // des données. Les pires cas fixes + varlena agressif maximisent buf.len().
        let batch: Vec<(ContentCoreStorageRow, ContentCoreVarlenOwned)> = (0..BATCH_SIZE)
            .map(|_| (worst_case_storage(), aggressive_varlena()))
            .collect();

        // Buffer unique, préalloué à TOTAL_CAP : conforme au chemin réel
        // (BatchRenderer::render_batch), pas une reconstitution par thread.
        let mut buf = String::with_capacity(CONTENT_CORE_TOTAL_CAP);

        // Compteur de records effectivement projetés, pour vérifier qu'aucun
        // n'est silencieusement sauté.
        let mut projected = 0usize;

        for (i, (storage, varlena)) in batch.iter().enumerate() {
            let cap_before = buf.capacity();
            buf.clear();

            ContentCoreProjection::render(storage, varlena, &mut buf);

            // ── Invariant no-realloc inter-itération (primaire) ───────────────
            // La capacité ne doit pas avoir crû, dès la première itération :
            // buf est déjà préalloué à TOTAL_CAP avant la boucle.
            assert_eq!(
                buf.capacity(),
                cap_before,
                "REALLOC détecté à l'itération {i} : capacité {} → {} après render(). \
                 DYNAMIC_CAP ({}) sous-estime le pire cas varlena.",
                cap_before,
                buf.capacity(),
                CONTENT_CORE_TOTAL_CAP
            );

            if i == 0 {
                // ── Correction HTML : vérification sur le premier fragment ────
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
                assert!(buf.starts_with("<!DOCTYPE html>"), "DOCTYPE manquant");
                assert!(
                    buf.trim_end().ends_with("</html>"),
                    "balise </html> manquante"
                );
            }

            projected += 1;
        }

        // ── Vérification de débit ─────────────────────────────────────────────
        // Tous les enregistrements du lot ont été projetés.
        assert_eq!(
            projected, BATCH_SIZE,
            "Débit incomplet : {projected}/{BATCH_SIZE} records projetés."
        );

        println!(
            "[stress] {BATCH_SIZE} records projetés — no-realloc inter-itération vérifié. \
             TOTAL_CAP = {CONTENT_CORE_TOTAL_CAP}B."
        );
    }
}
