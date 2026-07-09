// crates/shell/render/benches/hot_path_certify.rs
// Certification zéro-allocation du pipeline de rendu Marius.
//
// Ce binaire déclare CountingAlloc comme allocateur global.
// Chaque appel à alloc() incrémente un compteur atomique.
// L'invariant certifié : render_batch_pure() n'alloue PAS pendant son exécution
// une fois le buffer unique pré-chauffé (O(1) allocation initiale attendue).
//
// Ce binaire est intentionnellement séparé de hot_path_render pour garantir
// que les mesures de timing ne subissent aucune perturbation liée aux
// deux fetch_add atomiques par allocation introduits par CountingAlloc.
//
// ─── Exécution ────────────────────────────────────────────────────────────
//
//   cargo bench -p marius-render --bench hot_path_certify

mod counting_alloc;
use counting_alloc::CountingAlloc;

/// Allocateur global instrumenté — actif uniquement dans ce binaire.
/// Délègue à System, incrémente ALLOC_COUNT/ALLOC_BYTES à chaque alloc().
#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

use divan::counter::{BytesCount, ItemsCount};
use divan::{Bencher, black_box};

use marius_projection::Projection;
use marius_render::render_batch_pure;
use marius_schema::{
    CONTENT_CORE_TOTAL_CAP, ContentCoreProjection, ContentCoreStorageRow, ContentCoreVarlenOwned,
};

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
        published_at: 1_700_000_000_000_000i64,
        created_at: 1_700_000_000_000_000i64,
        modified_at: 1_700_000_000_000_000i64,
        document_id: 42i32,
        author_entity_id: 7i32,
        status: 1i16,
        is_readable: 0,
        is_commentable: 0,
        is_visible_comments: 0,
    };
    let varlena = ContentCoreVarlenOwned {
        headline: Some("Introduction à l'architecture DOD".to_string()),
        description: Some("Système de projection réactif AOT.".to_string()),
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
        published_at: i64::MIN,
        created_at: i64::MIN,
        modified_at: i64::MIN,
        document_id: i32::MIN,
        author_entity_id: i32::MIN,
        status: i16::MIN,
        is_readable: 0,
        is_commentable: 0,
        is_visible_comments: 0,
    };
    let varlena = ContentCoreVarlenOwned {
        headline: Some(aggressive.clone()),
        description: Some(aggressive.clone()),
        alternative_headline: Some(aggressive),
        ..Default::default()
    };
    (storage, varlena)
}

/// Lot de N enregistrements pour les benchmarks séquentiels.
/// `f` sélectionne le constructeur : record_nominal ou record_worst_case.
fn batch(
    size: usize,
    f: fn() -> (ContentCoreStorageRow, ContentCoreVarlenOwned),
) -> Vec<(ContentCoreStorageRow, ContentCoreVarlenOwned)> {
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
// III. Benchmarks pipeline séquentiel — granularité batch
// =============================================================================

/// Tailles de lot pour les benchmarks séquentiels.
/// 100  : lot petit, overhead d'itération dominant.
/// 1000 : lot moyen → cas nominal du Dispatcher.
/// 10000: lot large, calcul dominant → mesure le débit de saturation CPU.
const BATCH_SIZES: &[usize] = &[100, 1_000, 10_000];

/// Pipeline séquentiel avec données nominales — pattern exact du Dispatcher.
///
/// Reproduit fidèlement render_batch_pure() de dispatcher.rs :
///   buffer unique réutilisé, buf.clear() préserve la capacité entre
///   itérations, zéro allocation intra-lot après le premier render().
///
/// BytesCount = BATCH_SIZE × TOTAL_CAP : borne supérieure du HTML produit.
/// Le débit réel (MB/s) sera inférieur car les données nominales
/// n'atteignent pas TOTAL_CAP — c'est le débit de capacité, pas de remplissage.
#[divan::bench(
    name    = "render/sequential/nominal",
    args    = BATCH_SIZES,
)]
fn bench_render_sequential_nominal(bencher: Bencher, batch_size: usize) {
    bencher
        .counter(ItemsCount::new(batch_size))
        .counter(BytesCount::new(batch_size * CONTENT_CORE_TOTAL_CAP))
        .with_inputs(|| batch(batch_size, record_nominal))
        .bench_local_values(|records| {
            // render_batch_pure() : rendu séquentiel sans I/O disque.
            // Isole le coût CPU (marius_html_escape + push_str) des syscalls
            // write(2) qui domineraient la mesure (~22µs/record vs ~420ns/record).
            // black_box sur le batch entier : empêche LLVM d'éliminer render()
            // en prouvant que le contenu du buffer n'est pas observé.
            render_batch_pure::<ContentCoreProjection>(black_box(records));
        });
}

/// Pipeline séquentiel avec données pires cas.
///
/// Révèle la dégradation du débit sous charge maximale de marius_html_escape().
/// Le ratio time/nominal vs time/worst_case quantifie le surcoût de l'escape HTML
/// sur le chemin critique.
#[divan::bench(
    name    = "render/sequential/worst_case",
    args    = BATCH_SIZES,
)]
fn bench_render_sequential_worst_case(bencher: Bencher, batch_size: usize) {
    bencher
        .counter(ItemsCount::new(batch_size))
        .counter(BytesCount::new(batch_size * CONTENT_CORE_TOTAL_CAP))
        .with_inputs(|| batch(batch_size, record_worst_case))
        .bench_local_values(|records| {
            render_batch_pure::<ContentCoreProjection>(black_box(records));
        });
}

// =============================================================================
// Benchmark de certification zéro-allocation
// =============================================================================

/// Certifie que P::render() n'alloue pas sur le tas pendant son exécution.
///
/// ─── Périmètre de la certification ───────────────────────────────────────────
///
///   La fenêtre reset/read encadre un appel unique à render() avec un buffer
///   déjà alloué à TOTAL_CAP. Cette granularité est la seule correcte :
///   render_batch_pure() alloue un unique buffer (O(1)) en tête de lot —
///   cette allocation initiale est légitime et hors du périmètre certifié ici.
///   Ce que l'on certifie ici : render() lui-même, une fois le buffer stable.
///
/// ─── Protocole ───────────────────────────────────────────────────────────────
///
///   setup (hors fenêtre Divan, dans with_inputs) :
///     - buf alloué à TOTAL_CAP via String::with_capacity()
///     - un premier render() pour confirmer la capacité suffisante
///       (l'allocateur peut arrondir à la page supérieure — c'est acceptable)
///
///   fenêtre certifiée (dans bench_local_values) :
///     - buf.clear()            → len=0, capacity inchangée
///     - CountingAlloc::reset() → compteurs à 0, barrière SeqCst
///     - render(&record, &varlena, &mut buf)
///     - assert ALLOC_COUNT == 0
///
/// ─── Invariant prouvé ────────────────────────────────────────────────────────
///
///   buf.capacity() >= STATIC_CAP + DYNAMIC_CAP avant l'appel garantit que
///   buf.reserve() dans render() est un no-op. ALLOC_COUNT == 0 confirme
///   qu'aucun autre chemin dans render() n'alloue.
///
/// ─── Ce que ce test ne prouve pas ────────────────────────────────────────────
///
///   Il ne certifie pas render_batch_pure() dans son ensemble : l'allocation
///   initiale du buffer unique (avant la première itération) reste hors
///   fenêtre — comportement attendu, non instrumenté ici.
#[divan::bench(name = "certify/zero_alloc_in_render", sample_count = 100)]
fn bench_certify_zero_alloc(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            // with_inputs produit le tuple (storage, varlena, buf) à chaque sample.
            // Inclure storage et varlena dans le tuple est nécessaire pour que Divan
            // reconnaisse un input complet et produise un rapport de timing visible.
            // Les capturer hors de with_inputs comme références produit un affichage
            // tronqué : Divan ne voit pas d'input à mesurer et n'affiche pas les temps.
            //
            // buf est pré-chauffé ici (hors fenêtre de certification) : le premier
            // render() garantit que capacity >= TOTAL_CAP après l'éventuel arrondi
            // page de l'allocateur. Les allocations de ce setup sont hors reset/read.
            let (storage, varlena) = record_worst_case();
            let mut buf = String::with_capacity(CONTENT_CORE_TOTAL_CAP);
            ContentCoreProjection::render(&storage, &varlena, &mut buf);
            (storage, varlena, buf)
        })
        .bench_local_values(|(storage, varlena, mut buf)| {
            // ── Fenêtre de certification ──────────────────────────────────────
            // buf.clear() : len=0, capacity inchangée — buf est prêt pour render().
            buf.clear();
            // reset() : barrière SeqCst — garantit la visibilité avant render().
            CountingAlloc::reset();

            ContentCoreProjection::render(&storage, &varlena, &mut buf);

            // ── Lecture et assertion ──────────────────────────────────────────
            // SeqCst : garantit que toutes les écritures de render() sont visibles.
            let allocs = CountingAlloc::alloc_count();
            let bytes = CountingAlloc::alloc_bytes();

            assert_eq!(
                allocs, 0,
                "CERTIFICATION ÉCHOUÉE : {allocs} allocation(s) détectée(s) \
                 dans render() ({bytes} octets). \
                 DYNAMIC_CAP ({CONTENT_CORE_TOTAL_CAP}B) sous-estime le pire cas. \
                 Vérifier max_display_width (FieldKind) et max_escaped_len (VarlenField) \
                 dans crates/forge/fragment-forge/src/lib.rs."
            );

            // black_box sur la référence — force LLVM à considérer buf comme
            // observable sans retourner de valeur scalaire à Divan.
            // Retourner un usize (buf.len()) confond Divan qui l'interprète comme
            // un compteur d'items et supprime la ligne de timing dans le rapport.
            // La closure retourne () implicitement : Divan mesure le temps pur.
            black_box(&buf);
        });
}
