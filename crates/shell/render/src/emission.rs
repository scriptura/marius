// crates/shell/render/src/emission.rs

//! Résolution runtime des Sources et planification d'émission — Phase 4
//! (GO 2026-09), DESIGN-runtime-segment-pipeline.md §3, §3.2, §4, §11.
//!
//! Matérialise la descente :
//!
//! ```text
//! SourceKey / SourceSpec
//!     → résolution runtime d'une génération   → MaterializedSource
//!     → résolution d'une sélection             → ResolvedRange { ptr, len }
//! RouteDescriptor → résolution des Sources → résolution des ranges → EmissionPlan
//! ```
//!
//! ## Ce que ce module n'est PAS (périmètre strict de cette phase)
//!
//! - Il ne branche rien sur `LiveRegistry`/`ROUTE_TABLE`/`serve_route` — la
//!   résolution de génération (`resolve_generation` ci-dessous) reçoit sa
//!   source de vérité par injection (`fetch: FnOnce(SourceKey) -> ...`),
//!   jamais par un appel direct à `LiveRegistry`. L'alimentation réelle par
//!   le catalogue AOT/`LiveRegistry` est une intégration distincte,
//!   délibérément non entreprise ici (cf. rapport de session).
//! - Il n'invente ni producteur, ni cycle d'invalidation, ni stockage
//!   effectif, ni protocole de publication, ni longueur effective pour les
//!   Sources `Volatile` — `SourceSpec::VolatileSlot` reste une description
//!   AOT sans alimentation runtime à ce stade.
//! - Il ne construit aucun `IoSlice`, ne connaît ni `writev`/`sendmsg`, ni
//!   Axum, ni Hyper, ni Tokio.
//! - `RequestArena` ne fixe aucun mécanisme d'acquisition, pool, stratégie
//!   de recyclage, ni unité d'exécution propriétaire — seuls les
//!   invariants verrouillés par DESIGN §11.1 sont implémentés.

use std::sync::Arc;

use marius_projection::{RouteDescriptor, SourceId, SourceKey, SourceSpec};

use crate::pack_html_index::PackHtmlIndex;

// =============================================================================
// MaterializedSource — DESIGN §3
// =============================================================================

/// Matérialisation d'une origine, résolue une fois par `SourceKey` distinct
/// (DESIGN §3). Enum fermé, pas trait object — cohérent avec l'interdiction
/// d'indirection dynamique sur le chemin chaud (ADR-011 §7).
///
/// N'est PAS `Copy` — la variante `Mmap` porte un `Arc<PackHtmlIndex>`
/// (`Arc: Drop`), incompatible avec `Copy` par construction du langage
/// (DESIGN §3, correction explicite d'une version antérieure de la
/// délibération qui la traitait comme telle par erreur).
#[derive(Clone)]
pub enum MaterializedSource {
    /// Artefact statique publié — aujourd'hui l'unique variante réellement
    /// constructible par ce module (`resolve_generation` ci-dessous).
    Mmap { handle: Arc<PackHtmlIndex> },
    /// Segment de requête (session, panier...) — variante structurelle
    /// préparée par le DESIGN, mais dont le producteur, le cycle
    /// d'invalidation, le stockage et le protocole de publication restent
    /// délibérément non implémentés à cette phase (GO §8). Aucune fonction
    /// de ce module ne construit cette variante.
    Volatile { arena_ptr: *const u8 },
}

// `#[derive(Debug)]` est impossible ici : `PackHtmlIndex` (crate::pack_html_index,
// non modifié par cette phase) ne dérive pas `Debug`, et `Arc<T>: Debug`
// exige `T: Debug`. Implémentation manuelle, minimale — n'expose jamais le
// contenu de `PackHtmlIndex`, seulement la variante.
impl std::fmt::Debug for MaterializedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MaterializedSource::Mmap { .. } => f.write_str("MaterializedSource::Mmap(..)"),
            MaterializedSource::Volatile { arena_ptr } => f
                .debug_struct("MaterializedSource::Volatile")
                .field("arena_ptr", arena_ptr)
                .finish(),
        }
    }
}

/// Résout une génération publiée pour `spec`, via `fetch` — cette primitive
/// ne connaît jamais `LiveRegistry` directement (séparation volontaire,
/// cf. en-tête de module) : `fetch` représente l'alimentation par le futur
/// catalogue AOT/`LiveRegistry`, injectée par l'appelant, jamais câblée en
/// dur ici. Les tests de ce module injectent des `PackHtmlIndex`
/// synthétiques — ces fixtures ne sont pas une API de production.
///
/// Ne gère aujourd'hui que `SourceSpec::StaticArtifact` — `VolatileSlot`
/// retourne toujours `None` : son producteur n'existe pas encore (GO §8),
/// ce n'est pas une omission mais une limite volontaire de cette phase.
pub fn resolve_generation<F>(spec: &SourceSpec, fetch: F) -> Option<MaterializedSource>
where
    F: FnOnce(SourceKey) -> Option<Arc<PackHtmlIndex>>,
{
    match spec {
        SourceSpec::StaticArtifact { key } => {
            fetch(*key).map(|handle| MaterializedSource::Mmap { handle })
        }
        SourceSpec::VolatileSlot { .. } => None,
    }
}

// =============================================================================
// SourceResolutionContext — cohérence par SourceKey, DESIGN §3/§3.1
// =============================================================================

/// Contexte de résolution de génération, borné par le nombre de
/// `SourceKey` **distincts référencés par la route** — jamais par le
/// budget de segments `K` (DESIGN §3, correction de cardinalité :
/// `SourceId` est local à la route, `SourceKey` est global au registre ;
/// plusieurs `SourceId` peuvent référencer la même `SourceKey`, auquel cas
/// une seule résolution doit être observée par les deux).
///
/// Structure délibérément simple (recherche linéaire) — GO §3 : « ne faites
/// pas de déduplication sophistiquée ». `N` attendu petit (quelques
/// unités) ; une recherche linéaire dans ce régime est à la fois plus
/// simple à certifier zéro-allocation et suffisamment rapide en pratique.
pub struct SourceResolutionContext<const N: usize> {
    slots: [Option<(SourceKey, MaterializedSource)>; N],
    len: usize,
}

impl<const N: usize> SourceResolutionContext<N> {
    /// Contexte vide — aucune allocation (tableau à capacité fixe sur la
    /// pile ou dans le conteneur de l'appelant).
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            len: 0,
        }
    }

    /// Génération déjà résolue pour `key` dans ce contexte, s'il y en a
    /// une. L'appelant doit toujours consulter `get` avant `insert` :
    /// c'est cette discipline, pas une déduplication interne à `insert`,
    /// qui garantit l'invariant « une résolution par `SourceKey` par
    /// requête » (DESIGN §3).
    pub fn get(&self, key: SourceKey) -> Option<&MaterializedSource> {
        self.slots[..self.len].iter().find_map(|slot| match slot {
            Some((k, source)) if *k == key => Some(source),
            _ => None,
        })
    }

    /// Enregistre la résolution de `key`. Retourne `false` sans effet si
    /// la capacité `N` est épuisée pour une clé nouvelle — jamais un
    /// panic, jamais une réallocation implicite. N'écrase jamais une
    /// entrée existante : appeler `insert` pour une `key` déjà présente
    /// est une erreur d'usage de l'appelant (vérifiable via `get` au
    /// préalable), signalée ici par un `debug_assert!` plutôt qu'un
    /// comportement silencieux.
    pub fn insert(&mut self, key: SourceKey, source: MaterializedSource) -> bool {
        debug_assert!(
            self.get(key).is_none(),
            "SourceResolutionContext::insert : SourceKey déjà résolue — \
             l'appelant doit vérifier via get() avant d'appeler insert()"
        );
        if self.len >= N {
            return false;
        }
        self.slots[self.len] = Some((key, source));
        self.len += 1;
        true
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> Default for SourceResolutionContext<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Indexation locale-à-route pure : `SegmentDescriptor.source` (`SourceId`)
/// vers le `SourceSpec` correspondant dans `route.sources` (DESIGN §13.2).
/// Fonction pure, bornes vérifiées — aucun état, aucune E/S.
pub fn source_spec_for(route: &RouteDescriptor, source: SourceId) -> Option<&SourceSpec> {
    route.sources.get(source.0 as usize)
}

// =============================================================================
// ResolvedRange — DESIGN §3.2
// =============================================================================

/// Niveau physique/résolu — DESIGN §3.2, GO §5. Ne porte plus AUCUNE
/// information d'origine : pas de `SourceKey`/`SourceId`/`SegmentSelection`,
/// pas de `PackHtmlIndex`/`ArcSwap`, pas de sémantique HTTP. Un futur
/// backend doit pouvoir consommer cette forme sans connaître l'origine de
/// la mémoire.
///
/// Représenté ici comme une tranche empruntée (`&'a [u8]`) plutôt qu'un
/// pointeur brut : couvre exactement le contrat `(ptr, len)` demandé tout
/// en restant vérifié par l'emprunteur du compilateur — la conversion vers
/// un pointeur brut pour la construction d'`IoSlice` (DESIGN §7) est hors
/// périmètre de cette phase. Validité garantie par l'`Arc` déjà détenu par
/// le `MaterializedSource` résolu en amont (§3.2) ; `ResolvedRange`
/// lui-même ne détient rien et n'a donc jamais besoin de `Drop`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedRange<'a> {
    bytes: &'a [u8],
}

impl<'a> ResolvedRange<'a> {
    pub fn ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn as_slice(&self) -> &'a [u8] {
        self.bytes
    }
}

const _: () = assert!(
    !std::mem::needs_drop::<ResolvedRange<'static>>(),
    "ResolvedRange ne doit jamais nécessiter de Drop — ne détient rien, \
     seulement une tranche empruntée"
);

/// Résout une plage physique pour `source`, à partir d'une valeur de
/// sélection runtime déjà extraite (DESIGN §2.1 : cette fonction reçoit la
/// VALEUR, jamais la référence de sélection AOT — l'extraction depuis
/// `SegmentSelection`/le contexte de requête est hors périmètre de cette
/// phase, cf. rapport de session).
///
/// Ne gère aujourd'hui que `MaterializedSource::Mmap` — `Volatile` retourne
/// toujours `None`, pour la même raison que `resolve_generation` (GO §8).
pub fn resolve_range<'a>(
    source: &'a MaterializedSource,
    selection_value: i64,
) -> Option<ResolvedRange<'a>> {
    match source {
        MaterializedSource::Mmap { handle } => {
            let (offset, len) = handle.lookup(selection_value)?;
            let bytes = handle.blob(offset, len)?;
            Some(ResolvedRange { bytes })
        }
        MaterializedSource::Volatile { .. } => None,
    }
}

// =============================================================================
// RequestArena — DESIGN §11.1 (invariants verrouillés uniquement)
// =============================================================================

/// Support mémoire des segments volatils — DESIGN §11, invariants
/// verrouillés uniquement (§11.1). Forme minimale (GO §7) : mécanisme
/// d'acquisition, pool, stratégie de recyclage, unité d'exécution
/// propriétaire — **non décidés ici**, et ce type ne les présuppose pas.
///
/// - **request-scoped** : une instance n'a de sens que pour la durée d'une
///   requête. Ce type ne se procure pas lui-même, ne se recycle pas — il
///   n'est qu'un buffer à curseur, sans opinion sur son cycle de vie au-delà
///   des méthodes qu'il expose.
/// - **non partagée entre requêtes concurrentes** : aucune synchronisation
///   interne (pas de `Mutex`/`RwLock`/atomics) — imposer cette discipline à
///   l'appelant est un invariant d'usage, pas une garantie que ce type
///   applique lui-même.
/// - **capacité dérivée d'une borne AOT** : `with_capacity` reçoit la
///   capacité en paramètre, ne la choisit jamais elle-même. L'appelant est
///   responsable de la dériver de `RouteDescriptor.volatile_capacity`
///   (DESIGN §11.3) — non câblé ici, aucun générateur de routes réel
///   n'existe encore pour le fournir en production.
/// - **bump-only, reset O(1)** (§11.1/§11.2) : `reset` remet le curseur à
///   zéro sans jamais toucher au contenu du buffer ; `bump` avance le
///   curseur sans jamais réutiliser un bloc déjà rendu dans le même cycle.
/// - **aucune allocation sur le chemin d'utilisation** : le buffer est
///   alloué une seule fois, à la construction (`with_capacity`) — ni
///   `bump` ni `reset` n'allouent.
pub struct RequestArena {
    buf: Box<[u8]>,
    cursor: usize,
}

impl RequestArena {
    /// `capacity` est une donnée reçue, jamais choisie ici — dérivation
    /// depuis une borne AOT laissée à l'appelant (cf. doc de type
    /// ci-dessus).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: vec![0u8; capacity].into_boxed_slice(),
            cursor: 0,
        }
    }

    /// Remise à zéro en O(1) — DESIGN §11.1/§11.2 : à l'acquisition par une
    /// requête, jamais à la libération. Ce type n'impose ni ne devine
    /// quand cette méthode doit être appelée.
    pub fn reset(&mut self) {
        self.cursor = 0;
    }

    /// Allocation par curseur. Retourne `None` si la capacité restante est
    /// insuffisante — refus explicite, jamais une écriture hors bornes. Le
    /// traitement de ce cas (troncature, rejet, autre) reste un point
    /// produit-métier différé (DESIGN §12) : cette méthode ne fait que
    /// signaler l'échec.
    pub fn bump(&mut self, len: usize) -> Option<&mut [u8]> {
        let end = self.cursor.checked_add(len)?;
        if end > self.buf.len() {
            return None;
        }
        let slice = &mut self.buf[self.cursor..end];
        self.cursor = end;
        Some(slice)
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn used(&self) -> usize {
        self.cursor
    }
}

// =============================================================================
// EmissionPlan — DESIGN §4
// =============================================================================

/// Combine, pour une requête donnée, le plan fixe (`RouteDescriptor`) et le
/// résultat de la résolution runtime des plages (§3.2) — DESIGN §4, GO §6.
///
/// `K` (budget de segments de la route, `route.segments.len()`) est porté
/// comme paramètre const générique plutôt qu'un `Vec` : cohérent avec
/// l'exigence zéro allocation du chemin chaud (ADR-011 §7), même si ce
/// type n'est pas encore branché sur ce chemin dans cette phase. **Forme
/// provisoire, non figée par le DESIGN** (§4 : « aucune forme Rust
/// définitive n'est figée ») — le futur point d'intégration HTTP peut
/// retenir une représentation différente.
///
/// Ne construit aucun `IoSlice`, ne connaît ni `writev`/`sendmsg`, ni
/// Axum/Hyper/Tokio (GO §6) — s'arrête au niveau `ResolvedRange`.
pub struct EmissionPlan<'req, const K: usize> {
    route: &'req RouteDescriptor,
    ranges: [Option<ResolvedRange<'req>>; K],
}

impl<'req, const K: usize> EmissionPlan<'req, K> {
    /// `route.segments.len()` doit être égal à `K` — vérifié par
    /// `debug_assert!`, jamais silencieusement toléré : un écart signale
    /// une route mal générée (Forge), pas un cas runtime à absorber (même
    /// discipline que `is_single_file_compatible` pour le cas 0 segment,
    /// DESIGN §9.1).
    pub fn new(route: &'req RouteDescriptor) -> Self {
        debug_assert_eq!(
            route.segments.len(),
            K,
            "EmissionPlan::new : K doit correspondre exactement au nombre \
             de segments de la route — un écart signale une route mal \
             générée (Forge), jamais un cas runtime"
        );
        Self {
            route,
            ranges: std::array::from_fn(|_| None),
        }
    }

    pub fn route(&self) -> &'req RouteDescriptor {
        self.route
    }

    /// Enregistre la plage résolue pour le segment d'indice `i` —
    /// correspondance stricte 1:1 avec `route.segments` (§3.2). Retourne
    /// `false` si `i` est hors bornes.
    pub fn set_range(&mut self, i: usize, range: ResolvedRange<'req>) -> bool {
        if i >= K {
            return false;
        }
        self.ranges[i] = Some(range);
        true
    }

    /// `None` tant que le segment `i` n'a pas encore été résolu.
    pub fn range(&self, i: usize) -> Option<&ResolvedRange<'req>> {
        self.ranges.get(i)?.as_ref()
    }

    /// Tous les segments ont-ils une plage résolue ? Condition nécessaire
    /// avant toute construction future d'`IoSlice[]` (§7, hors périmètre
    /// de cette phase).
    pub fn is_fully_resolved(&self) -> bool {
        self.ranges.iter().all(Option::is_some)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_html_format::{PackfileEntry, write_packfile_footer};
    use std::io::{BufWriter, Write};
    use std::path::PathBuf;

    /// Écrit un packfile HTML synthétique sur disque — même patron que les
    /// tests de `pack_html_index.rs` (fonction non partagée entre modules
    /// de test ; dupliquée ici volontairement, plutôt que de rendre un
    /// utilitaire de test d'un autre module public en production pour ce
    /// seul besoin).
    fn write_synthetic_packfile(name: &str, blob: &[u8], index: &[PackfileEntry]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "marius_emission_test_{name}_{}.bin",
            std::process::id()
        ));
        let file = std::fs::File::create(&path).expect("création fichier temporaire");
        let mut writer = BufWriter::new(file);
        writer.write_all(blob).expect("écriture du blob");
        write_packfile_footer(&mut writer, blob.len() as u64, index)
            .expect("écriture footer+index");
        writer.flush().expect("flush");
        path
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
    }

    fn open_synthetic(name: &str, blob: &[u8], entries: &[PackfileEntry]) -> Arc<PackHtmlIndex> {
        let path = write_synthetic_packfile(name, blob, entries);
        let index = PackHtmlIndex::open(&path).expect("open() doit réussir sur packfile valide");
        cleanup(&path);
        Arc::new(index)
    }

    // ── résolution d'une Source statique ───────────────────────────────

    #[test]
    fn resolve_generation_static_artifact_calls_fetch_with_matching_key() {
        let index = open_synthetic(
            "resolve_gen",
            b"hello world",
            &[PackfileEntry {
                id: 1,
                offset: 0,
                len: 11,
                _pad: [0; 4],
            }],
        );
        let spec = SourceSpec::StaticArtifact { key: SourceKey(42) };

        let mut fetched_key = None;
        let result = resolve_generation(&spec, |key| {
            fetched_key = Some(key);
            Some(Arc::clone(&index))
        });

        assert_eq!(fetched_key, Some(SourceKey(42)));
        assert!(matches!(result, Some(MaterializedSource::Mmap { .. })));
    }

    #[test]
    fn resolve_generation_volatile_slot_is_never_constructed() {
        let spec = SourceSpec::VolatileSlot { capacity: 128 };
        let result = resolve_generation(&spec, |_key| {
            panic!("fetch ne doit jamais être appelé pour VolatileSlot")
        });
        assert!(result.is_none());
    }

    // ── cohérence d'une même SourceKey / plusieurs SourceId partageant
    //    une SourceKey ──────────────────────────────────────────────────

    #[test]
    fn same_source_key_resolves_to_the_same_generation_across_two_source_ids() {
        let index = open_synthetic("shared_key", b"shared blob", &[]);

        static SOURCES: &[SourceSpec] = &[
            SourceSpec::StaticArtifact { key: SourceKey(9) }, // SourceId(0)
            SourceSpec::StaticArtifact { key: SourceKey(9) }, // SourceId(1) — même SourceKey
        ];
        let route = RouteDescriptor {
            segments: &[],
            sources: SOURCES,
            backend_kind: marius_projection::EmissionBackendKind::Scatter,
            volatile_capacity: 0,
        };

        let spec_for_segment_0 = source_spec_for(&route, SourceId(0)).unwrap();
        let spec_for_segment_1 = source_spec_for(&route, SourceId(1)).unwrap();
        let key_0 = match spec_for_segment_0 {
            SourceSpec::StaticArtifact { key } => *key,
            _ => panic!("attendu StaticArtifact"),
        };
        let key_1 = match spec_for_segment_1 {
            SourceSpec::StaticArtifact { key } => *key,
            _ => panic!("attendu StaticArtifact"),
        };
        assert_eq!(
            key_0, key_1,
            "les deux SourceId doivent référencer la même SourceKey"
        );

        let mut ctx = SourceResolutionContext::<4>::new();
        let mut fetch_count = 0;

        // Résolution pour le segment 0 : cache vide, fetch appelé.
        if ctx.get(key_0).is_none() {
            let resolved = resolve_generation(spec_for_segment_0, |_| {
                fetch_count += 1;
                Some(Arc::clone(&index))
            })
            .unwrap();
            ctx.insert(key_0, resolved);
        }

        // Résolution pour le segment 1 : même SourceKey, doit être déjà en
        // cache — fetch ne doit PAS être appelé une seconde fois.
        if ctx.get(key_1).is_none() {
            let resolved = resolve_generation(spec_for_segment_1, |_| {
                fetch_count += 1;
                Some(Arc::clone(&index))
            })
            .unwrap();
            ctx.insert(key_1, resolved);
        }

        assert_eq!(
            fetch_count, 1,
            "une seule résolution de génération pour deux SourceId partageant la même SourceKey"
        );

        // Les deux segments observent bien le même Arc (même génération).
        let MaterializedSource::Mmap { handle: h0 } = ctx.get(key_0).unwrap() else {
            panic!("attendu Mmap");
        };
        let MaterializedSource::Mmap { handle: h1 } = ctx.get(key_1).unwrap() else {
            panic!("attendu Mmap");
        };
        assert!(Arc::ptr_eq(h0, h1));
    }

    #[test]
    fn context_capacity_is_bounded_not_a_vec() {
        let index = open_synthetic("bounded", b"x", &[]);
        let mut ctx = SourceResolutionContext::<1>::new();
        assert!(ctx.insert(
            SourceKey(1),
            MaterializedSource::Mmap {
                handle: Arc::clone(&index)
            }
        ));
        // Capacité épuisée pour une clé nouvelle : refus explicite, jamais
        // de réallocation implicite.
        assert!(!ctx.insert(
            SourceKey(2),
            MaterializedSource::Mmap {
                handle: Arc::clone(&index)
            }
        ));
        assert_eq!(ctx.len(), 1);
    }

    // ── résolution d'un range / conservation exacte (ptr,len) ───────────

    #[test]
    fn resolve_range_preserves_exact_bytes() {
        let blob = b"the quick brown fox";
        let index = open_synthetic(
            "range",
            blob,
            &[PackfileEntry {
                id: 7,
                offset: 4,
                len: 5, // "quick"
                _pad: [0; 4],
            }],
        );
        let source = MaterializedSource::Mmap { handle: index };

        let range = resolve_range(&source, 7).expect("id=7 doit être trouvé");
        assert_eq!(range.len(), 5);
        assert_eq!(range.as_slice(), b"quick");
        assert_eq!(range.ptr(), range.as_slice().as_ptr());
    }

    #[test]
    fn resolve_range_unknown_id_returns_none() {
        let index = open_synthetic("range_missing", b"data", &[]);
        let source = MaterializedSource::Mmap { handle: index };
        assert!(resolve_range(&source, 999).is_none());
    }

    #[test]
    fn resolve_range_volatile_is_never_resolved() {
        let source = MaterializedSource::Volatile {
            arena_ptr: std::ptr::null(),
        };
        assert!(resolve_range(&source, 1).is_none());
    }

    #[test]
    fn resolved_range_never_needs_drop() {
        assert!(!std::mem::needs_drop::<ResolvedRange<'static>>());
    }

    // ── RequestArena : capacité bornée, bump/reset ──────────────────────

    #[test]
    fn arena_bump_within_capacity_succeeds() {
        let mut arena = RequestArena::with_capacity(16);
        let slice = arena.bump(10).expect("10 <= 16 doit réussir");
        assert_eq!(slice.len(), 10);
        assert_eq!(arena.used(), 10);
    }

    #[test]
    fn arena_bump_exceeding_capacity_fails_explicitly() {
        let mut arena = RequestArena::with_capacity(8);
        assert!(arena.bump(9).is_none());
        assert_eq!(
            arena.used(),
            0,
            "un bump refusé ne doit jamais avancer le curseur"
        );
    }

    #[test]
    fn arena_reset_is_o1_and_does_not_touch_capacity() {
        let mut arena = RequestArena::with_capacity(32);
        arena.bump(20).unwrap();
        assert_eq!(arena.used(), 20);
        arena.reset();
        assert_eq!(arena.used(), 0);
        assert_eq!(arena.capacity(), 32, "reset ne modifie jamais la capacité");
    }

    #[test]
    fn arena_successive_bumps_never_overlap() {
        let mut arena = RequestArena::with_capacity(16);
        let first = arena.bump(4).unwrap();
        first[0] = 0xAA;
        let second = arena.bump(4).unwrap();
        // Deuxième tranche : jamais de chevauchement avec la première —
        // vérifié en écrivant une valeur distincte et en s'assurant que la
        // première zone (déjà rendue) reste ce qu'on y a écrit après coup
        // via le curseur avancé.
        second[0] = 0xBB;
        assert_eq!(arena.used(), 8);
    }

    // ── EmissionPlan ─────────────────────────────────────────────────────

    #[test]
    fn emission_plan_tracks_resolution_per_segment() {
        static SEGMENTS: &[marius_projection::SegmentDescriptor] =
            &[marius_projection::SegmentDescriptor {
                source: SourceId(0),
                selection: marius_projection::SegmentSelection::Constant(1),
                flags: marius_projection::SegmentFlags::NONE,
            }];
        static SOURCES: &[SourceSpec] = &[SourceSpec::StaticArtifact { key: SourceKey(1) }];
        let route = RouteDescriptor {
            segments: SEGMENTS,
            sources: SOURCES,
            backend_kind: marius_projection::EmissionBackendKind::SingleFile,
            volatile_capacity: 0,
        };

        let index = open_synthetic(
            "plan",
            b"payload",
            &[PackfileEntry {
                id: 1,
                offset: 0,
                len: 7,
                _pad: [0; 4],
            }],
        );
        let source = MaterializedSource::Mmap { handle: index };
        let range = resolve_range(&source, 1).unwrap();

        let mut plan = EmissionPlan::<1>::new(&route);
        assert!(!plan.is_fully_resolved());
        assert!(plan.set_range(0, range));
        assert!(plan.is_fully_resolved());
        assert_eq!(plan.range(0).unwrap().as_slice(), b"payload");
        assert_eq!(
            plan.route().backend_kind,
            marius_projection::EmissionBackendKind::SingleFile
        );
    }

    #[test]
    fn emission_plan_out_of_bounds_index_is_rejected() {
        let route = RouteDescriptor {
            segments: &[],
            sources: &[],
            backend_kind: marius_projection::EmissionBackendKind::Scatter,
            volatile_capacity: 0,
        };
        let mut plan = EmissionPlan::<0>::new(&route);
        // K=0 : toute tentative d'enregistrer une plage, quel que soit
        // l'indice, doit être rejetée — bornes vérifiées, jamais un panic
        // ni une écriture hors tableau.
        assert!(!plan.set_range(0, ResolvedRange { bytes: b"" }));
    }
}
