// crates/shell/render/benches/hot_path_render.rs

//! # marius-render - hot_path_render
//!
//! bench de rendu du chemin critique
//! Rendu zéro-allocation du pipeline de rendu Marius.
//!
//! Ce binaire ne contient AUCUN allocateur instrumenté.
//! L'allocateur système est utilisé tel quel : les mesures de débit
//! (GB/s, items/s) ne subissent aucune perturbation liée à l'instrumentation.
//!
//! Pour la certification zéro-allocation, utiliser le binaire séparé :
//!   cargo bench -p marius-render --bench hot_path_certify
//!
//! ## Granularités mesurées :
//!   render/single/*              : coût d'un render() unique, buffer isolé.
//!   render/sequential/nominal    : pipeline batch réel, données courtes.
//!   render/sequential/worst_case : pipeline batch réel, escape HTML saturé.
//!
//! ## Exécution :
//! `cargo bench -p marius-render --bench hot_path_render`

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
        walsn: 0,
        published_at: 1_700_000_000_000_000i64,
        created_at: 1_700_000_000_000_000i64,
        modified_at: 1_700_000_000_000_000i64,
        js_deps: 0,
        document_id: 42i32,
        author_entity_id: 7i32,
        status: 1i16,
        is_readable: 0,
        is_commentable: 0,
        is_visible_comments: 0,
        _pad: [0u8; 3],
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
        walsn: 0,
        published_at: i64::MIN,
        created_at: i64::MIN,
        modified_at: i64::MIN,
        js_deps: 0,
        document_id: i32::MIN,
        author_entity_id: i32::MIN,
        status: i16::MIN,
        is_readable: 0,
        is_commentable: 0,
        is_visible_comments: 0,
        _pad: [0u8; 3],
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

/// Coût brut de render_segments() sur un enregistrement nominal.
///
/// buf est recréé à chaque sample via with_inputs() — Divan isole le setup
/// hors de la fenêtre de mesure. black_box(buf) empêche LLVM d'éliminer
/// les push_str dont le résultat n'est pas observé.
///
/// Correction (23/07/2026) : appelait render() directement — cassé pour
/// content.core depuis que ce composant est segmenté (render() y est un
/// stub unreachable!(), CONTRAT-implementation-projection-segmentee.md
/// Étape 5). record_nominal() a is_readable=0 : le champ segmenté n'est de
/// toute façon jamais atteint ici — ce benchmark mesure le chemin non
/// segmenté du template, pas le mécanisme Segment lui-même (voir la section
/// dédiée « IV. Benchmarks chemin segmenté » plus bas pour ça).
#[divan::bench(name = "render/single/nominal")]
fn bench_render_single_nominal(bencher: Bencher) {
    let (storage, varlena) = record_nominal();

    bencher
        .counter(ItemsCount::new(1usize))
        .counter(BytesCount::new(CONTENT_CORE_TOTAL_CAP))
        .with_inputs(|| {
            (
                String::with_capacity(CONTENT_CORE_TOTAL_CAP),
                Vec::with_capacity(ContentCoreProjection::MAX_SEGMENTS),
            )
        })
        .bench_local_values(|(mut buf, mut segments)| {
            ContentCoreProjection::render_segments(&storage, &varlena, &mut buf, &mut segments);
            // black_box sur buf.len() ET segments.len() : force LLVM à
            // considérer les deux comme observables, empêchant l'élimination
            // du rendu ou des push() dans segments.
            black_box((buf.len(), segments.len()))
        });
}

/// Coût brut de render_segments() sur un enregistrement pire cas.
///
/// Mesure le chemin le plus long de marius_html_escape() :
/// toutes les branches activées, buf.len() proche de TOTAL_CAP.
/// Révèle le coût réel du pipeline en conditions dégradées.
///
/// Correction (23/07/2026) : même correctif que bench_render_single_nominal.
#[divan::bench(name = "render/single/worst_case")]
fn bench_render_single_worst_case(bencher: Bencher) {
    let (storage, varlena) = record_worst_case();

    bencher
        .counter(ItemsCount::new(1usize))
        .counter(BytesCount::new(CONTENT_CORE_TOTAL_CAP))
        .with_inputs(|| {
            (
                String::with_capacity(CONTENT_CORE_TOTAL_CAP),
                Vec::with_capacity(ContentCoreProjection::MAX_SEGMENTS),
            )
        })
        .bench_local_values(|(mut buf, mut segments)| {
            ContentCoreProjection::render_segments(&storage, &varlena, &mut buf, &mut segments);
            black_box((buf.len(), segments.len()))
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
// IV. Benchmarks chemin segmenté — CONTRAT-implementation-projection-segmentee.md
// =============================================================================
//
// Les fixtures record_nominal()/record_worst_case() ci-dessus ont
// is_readable=0 : la branche {% if %} qui contient le champ segmenté
// (content) n'est jamais atteinte à l'exécution — I/II/III ne mesurent que
// le chemin non segmenté du template. Cette section exerce spécifiquement
// le mécanisme Segment, ajoutée le 23/07/2026 en préparation d'une
// interruption prolongée de disponibilité (pas exécutée cette session).

/// Taille volontairement plus modeste que la borne DDL réelle (VARCHAR(2000000)) —
/// suffisant pour dépasser largement l'ancien seuil AOT de 64 Ko (donc pour
/// prouver le point), sans exiger des centaines de Mo de RAM cumulés sur les
/// lots de la section séquentielle ci-dessous (BATCH_SIZES × cette taille
/// serait ingérable à 10 000 × 2 000 000).
const SEGMENTED_BODY_LEN_TARGET: usize = 200_000;

/// Corps HTML volumineux représentatif d'un article réel — HTML déjà valide,
/// rien à échapper (EscapePolicy::Raw). Répétition d'un fragment court plutôt
/// qu'une seule chaîne géante : plus proche d'un contenu éditorial réel
/// (paragraphes), et garantit un nombre exact de caractères prévisible.
fn large_html_body() -> String {
    const FRAGMENT: &str = "<p>Paragraphe de test pour le contenu segmenté.</p>\n";
    FRAGMENT.repeat(SEGMENTED_BODY_LEN_TARGET / FRAGMENT.len() + 1)
}

/// Enregistrement avec champ segmenté actif (is_readable=1, condition réelle
/// du template core.marius) et corps volumineux emprunté — jamais concaténé
/// dans buf par construction (CONTRAT-implementation-projection-segmentee.md).
fn record_segmented_large() -> (ContentCoreStorageRow, ContentCoreVarlenOwned) {
    let storage = ContentCoreStorageRow {
        published_at: 1_700_000_000_000_000i64,
        created_at: 1_700_000_000_000_000i64,
        modified_at: 1_700_000_000_000_000i64,
        document_id: 7i32,
        author_entity_id: 3i32,
        status: 1i16,
        is_readable: 1, // active la branche {% if %} contenant le champ segmenté
        is_commentable: 0,
        is_visible_comments: 0,
        _pad: [0u8; 3],
    };
    let varlena = ContentCoreVarlenOwned {
        headline: Some("Article de test — contenu volumineux".to_string()),
        description: Some("Benchmark du chemin segmenté.".to_string()),
        alternative_headline: Some("Segmented Path Benchmark".to_string()),
        content: Some(large_html_body()),
        ..Default::default()
    };
    (storage, varlena)
}

/// Tailles de lot réduites par rapport à BATCH_SIZES (§III) — chaque
/// enregistrement porte ici un corps de ~200 Ko, contre quelques dizaines
/// d'octets pour les fixtures nominales/pire cas. 10 000 × 200 Ko = 2 Go,
/// ingérable en mémoire cumulée pour un simple benchmark ; 1 000 × 200 Ko =
/// 200 Mo reste raisonnable.
const SEGMENTED_BATCH_SIZES: &[usize] = &[10, 100, 1_000];

/// Coût d'un render_segments() unique avec un corps volumineux emprunté.
///
/// ─── Ce que ce benchmark doit révéler ────────────────────────────────────────
///
///   Un temps quasi identique à render/single/nominal (§II) — PAS
///   proportionnel à la taille de `content`. C'est précisément la promesse
///   du mécanisme : le corps n'est jamais copié dans `buf`, seulement
///   référencé (`Segment::Borrowed`, zéro copie). Si le temps mesuré ici
///   croît significativement avec SEGMENTED_BODY_LEN_TARGET, quelque chose
///   recopie encore le champ quelque part — régression à investiguer
///   immédiatement, pas un simple écart de performance à optimiser.
#[divan::bench(name = "render/segmented/single_large")]
fn bench_render_segmented_single_large(bencher: Bencher) {
    let (storage, varlena) = record_segmented_large();

    bencher
        .counter(ItemsCount::new(1usize))
        .counter(BytesCount::new(CONTENT_CORE_TOTAL_CAP))
        .with_inputs(|| {
            (
                String::with_capacity(CONTENT_CORE_TOTAL_CAP),
                Vec::with_capacity(ContentCoreProjection::MAX_SEGMENTS),
            )
        })
        .bench_local_values(|(mut buf, mut segments)| {
            ContentCoreProjection::render_segments(&storage, &varlena, &mut buf, &mut segments);
            // 3 segments attendus pour content.core (en-tête Buffered, corps
            // Borrowed, pied Buffered — MAX_SEGMENTS = 3 dans le code généré).
            // Échoue fort si le mécanisme ne s'est pas déclenché comme prévu,
            // plutôt que de laisser un chiffre de timing trompeur passer
            // silencieusement.
            assert_eq!(
                segments.len(),
                3,
                "3 segments attendus (en-tête/corps/pied), {} obtenus — le \
                 mécanisme de segmentation ne s'est peut-être pas déclenché \
                 (vérifier is_readable sur la fixture).",
                segments.len()
            );
            black_box((buf.len(), segments.len()))
        });
}

/// Pipeline séquentiel avec corps volumineux sur chaque enregistrement.
///
/// BytesCount ne compte que CONTENT_CORE_TOTAL_CAP × batch_size (le débit
/// « côté buf »), jamais la taille réelle des corps empruntés — cohérent
/// avec la promesse du mécanisme : le débit du chemin de rendu proprement
/// dit ne dépend pas de la taille des champs segmentés. Le débit global
/// d'écriture disque (corps compris) se mesure côté I/O réel
/// (BatchRenderer::render_batch, pack_html_format.rs) — hors périmètre de
/// render_batch_pure, qui ne touche jamais le filesystem par construction.
#[divan::bench(
    name    = "render/segmented/sequential_large",
    args    = SEGMENTED_BATCH_SIZES,
)]
fn bench_render_segmented_sequential_large(bencher: Bencher, batch_size: usize) {
    bencher
        .counter(ItemsCount::new(batch_size))
        .counter(BytesCount::new(batch_size * CONTENT_CORE_TOTAL_CAP))
        .with_inputs(|| batch(batch_size, record_segmented_large))
        .bench_local_values(|records| {
            render_batch_pure::<ContentCoreProjection>(black_box(records));
        });
}
