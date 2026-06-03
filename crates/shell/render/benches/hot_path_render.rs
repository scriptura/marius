// crates/shell/render/benches/hot_path_render.rs
// Micro-benchmarks Divan — pipeline de rendu Marius
//
// Deux granularités mesurées :
//
//   1. render_single_* : coût d'un render() unique, sans overhead Rayon.
//      Mesure le hot path brut : marius_html_escape + push_str + write_fmt.
//      Deux variantes : données courtes (cas nominal) et pires cas (DYNAMIC_CAP saturé).
//
//   2. render_rayon_batch_* : pattern map_with exact du Dispatcher.
//      Mesure la scalabilité parallèle sur les cœurs disponibles.
//      ItemsCount + BytesCount → débit en items/s et MB/s dans le terminal.
//
// ─── Indicateurs à surveiller ────────────────────────────────────────────────
//
//   Time/Item    : coût par enregistrement. Cible < 1µs sur données nominales.
//   Throughput   : MB/s de HTML produit. Révèle la saturation de la bande passante
//                  de calcul (L1/L2 cache) vs mémoire principale.
//   Variance     : proche de zéro → déterminisme du pipeline O(T) validé.
//                  Variance élevée → pression de l'allocateur ou contention Rayon.
//
// ─── Profil de compilation ───────────────────────────────────────────────────
//
//   cargo bench -p marius-render --bench hot_path_render
//   (profil release : inlining agressif, LLVM vectorise les push_str contigus)

use divan::{black_box, Bencher};
use divan::counter::{BytesCount, ItemsCount};
use rayon::prelude::*;

use marius_schema::{
    ContentCoreStorageRow,
    ContentCoreVarlenOwned,
    ContentCoreProjection,
    CONTENT_CORE_TOTAL_CAP,
};
use marius_projection::Projection;

fn main() {
    divan::main();
}

// =============================================================================
// I. Jeux de données
// =============================================================================

/// Enregistrement nominal : valeurs courtes, représentatives du cas moyen.
///
/// fixed-length : IDs et timestamps de l'ordre de 2023 (7-19 chars chacun).
/// varlena      : titre et description courts, sans caractère dangereux.
/// Ratio de remplissage attendu : ~5-15% de TOTAL_CAP (conforme ADR-003).
fn record_nominal() -> (ContentCoreStorageRow, ContentCoreVarlenOwned) {
    let storage = ContentCoreStorageRow {
        published_at:        1_700_000_000_000_000i64,
        created_at:          1_700_000_000_000_000i64,
        modified_at:         1_700_000_000_000_000i64,
        document_id:         42i32,
        author_entity_id:    7i32,
        status:              1i16,
        is_readable:         true,
        is_commentable:      true,
        is_visible_comments: true,
    };
    let varlena = ContentCoreVarlenOwned {
        headline:             Some("Introduction à l'architecture DOD".to_string()),
        description:          Some("Système de projection réactif AOT.".to_string()),
        alternative_headline: Some("Marius Engine".to_string()),
        ..Default::default()
    };
    (storage, varlena)
}

/// Enregistrement pire cas : maximise buf.len() → sature DYNAMIC_CAP.
///
/// fixed-length : valeurs minimales (i64::MIN = 20 chars, i32::MIN = 11 chars…).
/// varlena      : chaîne agressive, tous les caractères HTML dangereux.
///   '&' → "&amp;" (×5), '<' → "&lt;" (×3), '>' → "&gt;" (×3),
///   '"' → "&quot;" (×6), '\'' → "&#39;" (×5).
///   Répétée pour saturer max_escaped_len sans le dépasser.
///
/// Ce jeu de données prouve que LLVM ne vectorise pas render() en éliminant
/// le travail réel (les branches de marius_html_escape sont toutes activées).
fn record_worst_case() -> (ContentCoreStorageRow, ContentCoreVarlenOwned) {
    // Chaîne exercant toutes les branches de marius_html_escape().
    // Chaque caractère est dangereux → facteur d'escape maximal effectif.
    let aggressive = r#"<html> & "Marius" & 'Engine'</html>"#.repeat(6);

    let storage = ContentCoreStorageRow {
        published_at:        i64::MIN,
        created_at:          i64::MIN,
        modified_at:         i64::MIN,
        document_id:         i32::MIN,
        author_entity_id:    i32::MIN,
        status:              i16::MIN,
        is_readable:         false,
        is_commentable:      false,
        is_visible_comments: false,
    };
    let varlena = ContentCoreVarlenOwned {
        headline:             Some(aggressive.clone()),
        description:          Some(aggressive.clone()),
        alternative_headline: Some(aggressive),
        ..Default::default()
    };
    (storage, varlena)
}

/// Lot de N enregistrements pour les benchmarks Rayon.
/// `f` sélectionne le constructeur : record_nominal ou record_worst_case.
fn batch(size: usize, f: fn() -> (ContentCoreStorageRow, ContentCoreVarlenOwned))
    -> Vec<(ContentCoreStorageRow, ContentCoreVarlenOwned)>
{
    (0..size).map(|_| f()).collect()
}

// =============================================================================
// II. Benchmarks render() — granularité enregistrement unique
// =============================================================================

/// Coût brut de render() sur un enregistrement nominal.
///
/// buf est recréé à chaque sample via with_inputs() — Divan isole le setup
/// hors de la fenêtre de mesure. black_box(buf) empêche LLVM d'éliminer
/// les push_str dont le résultat n'est pas observé.
#[divan::bench(name = "render/single/nominal")]
fn bench_render_single_nominal(bencher: Bencher) {
    let (storage, varlena) = record_nominal();

    bencher
        .counter(ItemsCount::new(1usize))
        .counter(BytesCount::new(CONTENT_CORE_TOTAL_CAP))
        .with_inputs(|| String::with_capacity(CONTENT_CORE_TOTAL_CAP))
        .bench_local_values(|mut buf| {
            ContentCoreProjection::render(&storage, &varlena, &mut buf);
            // black_box sur buf.len() : force LLVM à considérer le contenu
            // du buffer comme observable, empêchant l'élimination du rendu.
            black_box(buf.len())
        });
}

/// Coût brut de render() sur un enregistrement pire cas.
///
/// Mesure le chemin le plus long de marius_html_escape() :
/// toutes les branches activées, buf.len() proche de TOTAL_CAP.
/// Révèle le coût réel du pipeline en conditions dégradées.
#[divan::bench(name = "render/single/worst_case")]
fn bench_render_single_worst_case(bencher: Bencher) {
    let (storage, varlena) = record_worst_case();

    bencher
        .counter(ItemsCount::new(1usize))
        .counter(BytesCount::new(CONTENT_CORE_TOTAL_CAP))
        .with_inputs(|| String::with_capacity(CONTENT_CORE_TOTAL_CAP))
        .bench_local_values(|mut buf| {
            ContentCoreProjection::render(&storage, &varlena, &mut buf);
            black_box(buf.len())
        });
}

// =============================================================================
// III. Benchmarks pipeline Rayon — granularité batch
// =============================================================================

/// Tailles de lot pour les benchmarks Rayon.
/// 100  : lot petit, overhead Rayon dominant → révèle le coût de distribution.
/// 1000 : lot moyen, équilibre overhead/calcul → cas nominal du Dispatcher.
/// 10000: lot large, calcul dominant → mesure le débit de saturation CPU.
const BATCH_SIZES: &[usize] = &[100, 1_000, 10_000];

/// Pipeline Rayon avec données nominales — pattern exact du Dispatcher.
///
/// Reproduit fidèlement map_with de dispatcher.rs :
///   seed = (String::new(), 0usize)  →  (buffer réutilisé, ref_cap)
///   buf.clear() préserve la capacité entre itérations.
///
/// BytesCount = BATCH_SIZE × TOTAL_CAP : borne supérieure du HTML produit.
/// Le débit réel (MB/s) sera inférieur car les données nominales
/// n'atteignent pas TOTAL_CAP — c'est le débit de capacité, pas de remplissage.
#[divan::bench(
    name    = "render/rayon/nominal",
    args    = BATCH_SIZES,
)]
fn bench_render_rayon_nominal(bencher: Bencher, batch_size: usize) {
    bencher
        .counter(ItemsCount::new(batch_size))
        .counter(BytesCount::new(batch_size * CONTENT_CORE_TOTAL_CAP))
        .with_inputs(|| batch(batch_size, record_nominal))
        .bench_local_values(|records| {
            records
                .into_par_iter()
                .map_with(
                    (String::new(), 0usize),
                    |(buf, ref_cap), (storage, varlena)| {
                        buf.clear();
                        ContentCoreProjection::render(&storage, &varlena, buf);

                        // Capture de la capacité de référence au premier rendu.
                        // Invariant no-realloc : vérifié dans le test d'intégration,
                        // pas ici (les assertions ralentissent le benchmark).
                        if *ref_cap == 0 {
                            *ref_cap = buf.capacity();
                        }

                        // black_box sur le contenu du buffer :
                        // empêche LLVM de prouver statiquement que buf est jeté
                        // et d'éliminer render() par dead-code elimination.
                        black_box(buf.len())
                    },
                )
                // Consommation de l'itérateur — for_each(|_| {}) est le pattern
                // standard pour map_with sans collecte.
                .for_each(|_| {});
        });
}

/// Pipeline Rayon avec données pires cas.
///
/// Révèle la dégradation du débit sous charge maximale de marius_html_escape().
/// Le ratio time/nominal vs time/worst_case quantifie le surcoût de l'escape HTML
/// sur le chemin critique.
#[divan::bench(
    name    = "render/rayon/worst_case",
    args    = BATCH_SIZES,
)]
fn bench_render_rayon_worst_case(bencher: Bencher, batch_size: usize) {
    bencher
        .counter(ItemsCount::new(batch_size))
        .counter(BytesCount::new(batch_size * CONTENT_CORE_TOTAL_CAP))
        .with_inputs(|| batch(batch_size, record_worst_case))
        .bench_local_values(|records| {
            records
                .into_par_iter()
                .map_with(
                    (String::new(), 0usize),
                    |(buf, ref_cap), (storage, varlena)| {
                        buf.clear();
                        ContentCoreProjection::render(&storage, &varlena, buf);
                        if *ref_cap == 0 {
                            *ref_cap = buf.capacity();
                        }
                        black_box(buf.len())
                    },
                )
                .for_each(|_| {});
        });
}
