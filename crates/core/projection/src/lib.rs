// crates/core/projection/src/lib.rs

//! # marius-projection
//! Trait Projection — interface canonique entre l'Orchestrator et les
//! implémentations générées par Bridge-Forge + Fragment-Forge.
//!
//! Ce crate est la frontière Core/Shell :
//!   - Il référence sqlx::PgPool (Shell) pour fetch_batch
//!   - Il porte PackfileStoreHeader + align8 : source de vérité unique du
//!     protocole binaire (partagée par PackfileBuilder et PackfileReader).
//!   - Phase 2 : PackfileReader exposé ici pour que marius_schema puisse
//!     l'utiliser sans cycle de dépendance (marius_render → marius_schema).
//!
//! ─── ADR-003 : Dualité Record / VarlenOwned ───────────────────────────────────
//!
//!   Record      : struct #[repr(C)], fixed-length, layout miroir PostgreSQL.
//!   VarlenOwned : struct possédée portant les données varlena (Option<String>).
//!                 () pour les tables sans varlena.
//!
//! ─── Protocole binaire ────────────────────────────────────────────────────────
//!
//!   Défini ici (PackfileStoreHeader, align8) — importé par PackfileBuilder
//!   (marius_render) et PackfileReader (ce crate). Toute modification du layout
//!   se propage automatiquement aux deux côtés.

use std::path::PathBuf;

// ─── SourceKey — identité canonique AOT (Phase 1, GO 2026-09) ───────────────
//
// Primitive d'identité pour le futur catalogue de sources généré par la
// Forge (ADR-011 §3 : ontologie Projection/Artefact/Segment). Cette phase
// n'introduit RIEN d'autre : ni catalogue (`SOURCE_CATALOG` écrit à la main
// explicitement rejeté — le catalogue sera une sortie AOT de la Forge, pas
// une quatrième copie manuelle de l'identité, à côté de
// `RouteEntry::packfile_key`/`ShardMetadata::packfile_key`/
// `Dispatcher::packfile_key`), ni association vers ces `packfile_key`
// existants, ni méthode de résolution. `SourceKey` reste un identifiant
// opaque, seul, en attendant la conception du catalogue lui-même.
//
// Aucune dépendance vers `marius-render`/`marius-server` : ce type ne
// connaît ni `RouteEntry` ni `LiveRegistry`, et ne doit jamais en connaître
// — c'est la frontière Forge/Runtime (cf. inventaire architectural,
// session ADR-011/SourceKey). `marius_projection` est déjà dépendu par les
// deux côtés (`marius-render` directement, `marius-server` via
// `marius-schema`), donc ce placement n'ajoute aucune arête de dépendance
// nouvelle au graphe du workspace.
/// Identifiant opaque d'une source, attribué une fois par la Forge (AOT).
///
/// `#[repr(transparent)]` : layout strictement identique à `u16` — aucun
/// octet de padding, aucune indirection, castable sans coût vers/depuis sa
/// représentation entière si un futur format binaire AOT (catalogue généré,
/// `SegmentDescriptor`) en a besoin. `pub u16` (champ nommé, pas de
/// constructeur dédié) : la Forge est la seule attendue à construire des
/// valeurs de ce type — pas de logique d'invariant à protéger derrière un
/// constructeur privé à ce stade (aucune valeur n'est encore réservée ou
/// interdite).
///
/// Volontairement sans aucune méthode : ni `From<SourceKey> for &'static
/// str`, ni résolution vers un chemin, ni vers une entrée de `LiveRegistry`.
/// La résolution appartient au futur catalogue (Forge) et à la projection
/// runtime qui en découlera — pas à ce type, qui ne doit rester qu'une
/// étiquette comparable.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SourceKey(pub u16);

// Assertions de layout — compile-time, même discipline que
// `PackfileEntry`/`PackfileFooter` (pack_html_format.rs) : une régression
// de représentation (ajout de champ, changement de repr) casse la build,
// jamais découverte au premier usage FFI/mmap/catalogue binaire.
const _: () = assert!(
    std::mem::size_of::<SourceKey>() == std::mem::size_of::<u16>(),
    "SourceKey doit avoir exactement la taille de u16 (repr(transparent))"
);
const _: () = assert!(
    std::mem::size_of::<SourceKey>() == 2,
    "SourceKey doit occuper exactement 2 octets"
);
const _: () = assert!(
    std::mem::align_of::<SourceKey>() == std::mem::align_of::<u16>(),
    "SourceKey doit avoir l'alignement de u16"
);

#[cfg(test)]
mod tests_source_key {
    use super::SourceKey;

    // Layout — redondant avec les assertions const ci-dessus (qui suffisent
    // à elles seules à faire échouer la build), reproduit ici en test pour
    // que la propriété apparaisse dans la sortie de `cargo test`, lisible
    // sans avoir à chercher les `const _: ()` du module.
    #[test]
    fn layout_is_exactly_two_bytes_aligned_as_u16() {
        assert_eq!(std::mem::size_of::<SourceKey>(), 2);
        assert_eq!(
            std::mem::align_of::<SourceKey>(),
            std::mem::align_of::<u16>()
        );
    }

    // Copy — vérifié par l'usage, pas par introspection : si SourceKey
    // n'était pas Copy, `a` serait déplacé par `let b = a;` et la seconde
    // lecture de `a` ci-dessous ne compilerait pas.
    #[test]
    fn is_copy_not_move() {
        let a = SourceKey(7);
        let b = a;
        assert_eq!(a, b);
        assert_eq!(a, SourceKey(7)); // `a` réutilisé après `b` — exige Copy
    }

    #[test]
    fn equality_is_by_value() {
        assert_eq!(SourceKey(0), SourceKey(0));
        assert_ne!(SourceKey(0), SourceKey(1));
    }

    #[test]
    fn ord_matches_underlying_u16() {
        assert!(SourceKey(1) < SourceKey(2));
        assert!(SourceKey(u16::MAX) > SourceKey(0));

        let mut keys = vec![SourceKey(3), SourceKey(1), SourceKey(2)];
        keys.sort();
        assert_eq!(keys, vec![SourceKey(1), SourceKey(2), SourceKey(3)]);
    }

    // Hash — condition nécessaire pour servir de clé dans le futur
    // catalogue (HashMap ou table indexée), sans présumer laquelle des deux
    // sera retenue (question explicitement hors périmètre de cette phase).
    #[test]
    fn usable_as_hashmap_key() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert(SourceKey(42), "quarante-deux");
        assert_eq!(map.get(&SourceKey(42)), Some(&"quarante-deux"));
        assert_eq!(map.get(&SourceKey(43)), None);
    }

    #[test]
    fn debug_format_is_available() {
        // Aucune assertion sur le format exact (non contractuel) — seule
        // l'existence de l'impl Debug (dérivée) est sous test ici.
        let _ = format!("{:?}", SourceKey(1));
    }
}

// ─── SourceId — identité locale à une route (Phase 2, GO 2026-09) ──────────
//
// Distinction à ne jamais confondre avec `SourceKey` ci-dessus (DESIGN
// runtime-segment-pipeline post-ADR-011, §13.2) :
//   - `SourceKey` : portée globale au catalogue — une valeur stable par
//     artefact nommé, à travers toutes les routes qui le référencent.
//   - `SourceId`  : portée locale à une route — un indice dans le
//     `sources: &[SourceSpec]` (à venir) de CETTE route uniquement. Deux
//     routes différentes peuvent réutiliser la même valeur numérique de
//     `SourceId` pour désigner des origines complètement différentes.
//
// Mêmes conventions que `SourceKey` (repr, dérivations, absence de
// méthode) — la différence entre les deux types est une différence de
// PORTÉE, jamais de représentation ou de comportement. Zéro consommateur à
// ce stade : ni `SourceSpec` (ci-dessous) ni aucune future
// `SegmentDescriptor`/`RouteDescriptor` n'y sont introduits dans cette
// phase (Phase 3/4 du séquencement révisé).
/// Identifiant local à une route, désignant un indice dans la table de
/// sources de CETTE route — jamais une identité globale (`SourceKey`).
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SourceId(pub u16);

const _: () = assert!(
    std::mem::size_of::<SourceId>() == std::mem::size_of::<u16>(),
    "SourceId doit avoir exactement la taille de u16 (repr(transparent))"
);
const _: () = assert!(
    std::mem::size_of::<SourceId>() == 2,
    "SourceId doit occuper exactement 2 octets"
);
const _: () = assert!(
    std::mem::align_of::<SourceId>() == std::mem::align_of::<u16>(),
    "SourceId doit avoir l'alignement de u16"
);

#[cfg(test)]
mod tests_source_id {
    use super::SourceId;

    #[test]
    fn layout_is_exactly_two_bytes_aligned_as_u16() {
        assert_eq!(std::mem::size_of::<SourceId>(), 2);
        assert_eq!(
            std::mem::align_of::<SourceId>(),
            std::mem::align_of::<u16>()
        );
    }

    #[test]
    fn is_copy_not_move() {
        let a = SourceId(3);
        let b = a;
        assert_eq!(a, b);
        assert_eq!(a, SourceId(3)); // `a` réutilisé après `b` — exige Copy
    }

    #[test]
    fn equality_is_by_value() {
        assert_eq!(SourceId(0), SourceId(0));
        assert_ne!(SourceId(0), SourceId(1));
    }

    #[test]
    fn ord_matches_underlying_u16() {
        assert!(SourceId(1) < SourceId(2));

        let mut ids = vec![SourceId(2), SourceId(0), SourceId(1)];
        ids.sort();
        assert_eq!(ids, vec![SourceId(0), SourceId(1), SourceId(2)]);
    }

    #[test]
    fn usable_as_hashmap_key() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert(SourceId(5), "cinq");
        assert_eq!(map.get(&SourceId(5)), Some(&"cinq"));
        assert_eq!(map.get(&SourceId(6)), None);
    }

    #[test]
    fn debug_format_is_available() {
        let _ = format!("{:?}", SourceId(1));
    }

    // Distinction de portée avec SourceKey : deux valeurs numériques
    // identiques restent des types distincts, non comparables entre eux —
    // le compilateur, pas un test, est la garantie ici (`SourceId(1) ==
    // SourceKey(1)` ne compile pas). Rien à exécuter, seulement à ne pas
    // regretter l'absence d'un From/Into entre les deux — volontairement
    // absent (cf. commentaire de section ci-dessus).
}

// ─── SourceSpec — recette de résolution d'un SourceId (Phase 2, GO 2026-09) ─
//
// DESIGN runtime-segment-pipeline post-ADR-011, §13.2. Table de résolution
// *par route* (à venir : `RouteDescriptor.sources: &[SourceSpec]`, Phase 4
// du séquencement révisé) indexée par `SourceId` : dit COMMENT matérialiser
// une origine logique, jamais où — la résolution effective
// (`SourceKey → Arc<PackHtmlIndex>` via `LiveRegistry`, ou réservation dans
// une future `RequestArena`) reste hors périmètre de cette phase et de ce
// type lui-même.
//
// Volontairement PAS de `#[repr(C)]` à ce stade, malgré la présence de
// cette annotation dans le DESIGN de référence. Aucune nécessité démontrée
// ne le justifie encore : `SourceSpec` n'est consommé par aucun code, ne
// traverse aucune frontière FFI/mmap, n'entre dans aucune table binaire
// générée — les trois raisons qui justifieraient de figer une
// représentation explicite (cf. `PackfileEntry`/`PackfileFooter`,
// `SourceKey`/`SourceId` eux-mêmes une fois `#[repr(transparent)]` motivé
// par leur rôle de futur indice compact). Un `#[repr(C)]` ajouté par
// anticipation ici serait une assertion de layout sans preuve du besoin —
// à revisiter explicitement quand `RouteDescriptor`/la génération AOT du
// catalogue (Phase 4+) donneront une raison concrète de figer la
// représentation mémoire de cet enum.
//
// Propriétés requises, vérifiées par construction du type plutôt que par
// assertion de layout : `Copy` (dérivé — chaque variante ne contient que
// des types `Copy`) ; aucun pointeur ni référence (aucun champ `&`/`*` —
// `SourceKey` est lui-même `Copy`/POD, `u32` est un scalaire) ; aucune
// allocation (aucun `Vec`/`String`/`Box` possible, la vérification
// `needs_drop` ci-dessous le confirme indirectement) ; type borné (enum
// fermé à deux variantes, pas de générique ouvert, pas de `dyn`).
/// Comment matérialiser l'origine désignée par un `SourceId`, au sein d'une
/// route — jamais où la trouver dans l'absolu (cf. `SourceKey`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceSpec {
    /// Origine résolue via le catalogue global — `SourceKey` à résoudre en
    /// `Arc<PackHtmlIndex>` via `LiveRegistry` (mécanisme de résolution
    /// hors périmètre de ce type et de cette phase).
    StaticArtifact { key: SourceKey },
    /// Origine à réserver dans l'arène de requête — capacité maximale
    /// connue à la compilation (Forge), longueur effective connue à la
    /// matérialisation (hors périmètre de ce type et de cette phase).
    VolatileSlot { capacity: u32 },
}

// Absence de Drop — propriété explicitement requise (liste de vérification
// de cette phase), vérifiée directement plutôt que déduite : si une
// variante future acquérait un jour un champ non trivial à détruire, cette
// assertion romprait la build avant tout usage problématique sur un chemin
// sans allocation.
const _: () = assert!(
    !std::mem::needs_drop::<SourceSpec>(),
    "SourceSpec ne doit jamais nécessiter de Drop"
);

#[cfg(test)]
mod tests_source_spec {
    use super::{SourceId, SourceKey, SourceSpec};

    // Copy — même méthode qu'ailleurs dans ce module : vérifiée par
    // l'usage (réutilisation après un déplacement apparent), pas par
    // introspection.
    #[test]
    fn is_copy_not_move() {
        let a = SourceSpec::StaticArtifact { key: SourceKey(9) };
        let b = a;
        assert_eq!(a, b);
        assert_eq!(a, SourceSpec::StaticArtifact { key: SourceKey(9) }); // `a` réutilisé après `b` — exige Copy
    }

    #[test]
    fn equality_is_structural_per_variant() {
        assert_eq!(
            SourceSpec::StaticArtifact { key: SourceKey(1) },
            SourceSpec::StaticArtifact { key: SourceKey(1) }
        );
        assert_ne!(
            SourceSpec::StaticArtifact { key: SourceKey(1) },
            SourceSpec::StaticArtifact { key: SourceKey(2) }
        );
        assert_eq!(
            SourceSpec::VolatileSlot { capacity: 64 },
            SourceSpec::VolatileSlot { capacity: 64 }
        );
        assert_ne!(
            SourceSpec::VolatileSlot { capacity: 64 },
            SourceSpec::VolatileSlot { capacity: 128 }
        );
    }

    // Deux variantes ne sont jamais égales, quelles que soient les valeurs
    // portées — dérivation par défaut de PartialEq sur un enum (le
    // discriminant de variante fait partie de la comparaison), vérifié
    // explicitement plutôt que supposé.
    #[test]
    fn different_variants_are_never_equal() {
        assert_ne!(
            SourceSpec::StaticArtifact { key: SourceKey(0) },
            SourceSpec::VolatileSlot { capacity: 0 }
        );
    }

    #[test]
    fn pattern_matching_recovers_the_carried_value() {
        let static_spec = SourceSpec::StaticArtifact { key: SourceKey(42) };
        match static_spec {
            SourceSpec::StaticArtifact { key } => assert_eq!(key, SourceKey(42)),
            SourceSpec::VolatileSlot { .. } => panic!("mauvaise variante"),
        }

        let volatile_spec = SourceSpec::VolatileSlot { capacity: 4096 };
        match volatile_spec {
            SourceSpec::VolatileSlot { capacity } => assert_eq!(capacity, 4096),
            SourceSpec::StaticArtifact { .. } => panic!("mauvaise variante"),
        }
    }

    // Aucune assertion de taille ici (cf. commentaire de section : pas de
    // `#[repr(C)]` sans nécessité démontrée) — seulement l'absence de
    // Drop, propriété explicitement requise pour cette phase.
    #[test]
    fn never_needs_drop() {
        assert!(!std::mem::needs_drop::<SourceSpec>());
    }

    // Table indexée par SourceId — usage attendu une fois RouteDescriptor
    // introduit (Phase 4), exercé ici au niveau le plus simple possible
    // (un slice, pas encore une structure dédiée) pour vérifier que rien
    // dans SourceId/SourceSpec n'empêche cet usage.
    #[test]
    fn indexable_by_source_id_in_a_plain_slice() {
        let sources = [
            SourceSpec::StaticArtifact { key: SourceKey(10) },
            SourceSpec::VolatileSlot { capacity: 256 },
        ];
        let cart_id = SourceId(1);
        assert_eq!(
            sources[cart_id.0 as usize],
            SourceSpec::VolatileSlot { capacity: 256 }
        );
    }
}

// ─── RequestValueId — référence opaque à un slot du contexte de requête
//     (Phase 3, GO 2026-09) ──────────────────────────────────────────────
//
// DESIGN runtime-segment-pipeline post-ADR-011, §2.1. Nom provisoire retenu
// tel quel depuis la délibération (handoff-checkpoint-segment-resolution.md
// §I) : un indice désignant « la valeur au slot N du contexte de requête »,
// sans qu'aucune sémantique HTTP ne soit connue ici. La correspondance
// « le slot N est rempli par le paramètre :id de l'URL » reste entièrement
// extérieure à ce crate — table compagnon du futur Request Context (Phase
// 4), jamais ici. Mêmes conventions que SourceKey/SourceId (repr, dérivations,
// absence de méthode de résolution) : un identifiant opaque, seul.
/// Référence opaque à un emplacement du contexte de requête — jamais une
/// valeur HTTP en elle-même (cf. `SegmentSelection` ci-dessous).
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RequestValueId(pub u16);

const _: () = assert!(
    std::mem::size_of::<RequestValueId>() == std::mem::size_of::<u16>(),
    "RequestValueId doit avoir exactement la taille de u16 (repr(transparent))"
);
const _: () = assert!(
    std::mem::align_of::<RequestValueId>() == std::mem::align_of::<u16>(),
    "RequestValueId doit avoir l'alignement de u16"
);

#[cfg(test)]
mod tests_request_value_id {
    use super::RequestValueId;

    #[test]
    fn layout_is_exactly_two_bytes_aligned_as_u16() {
        assert_eq!(std::mem::size_of::<RequestValueId>(), 2);
        assert_eq!(
            std::mem::align_of::<RequestValueId>(),
            std::mem::align_of::<u16>()
        );
    }

    #[test]
    fn is_copy_not_move() {
        let a = RequestValueId(4);
        let b = a;
        assert_eq!(a, b);
        assert_eq!(a, RequestValueId(4)); // `a` réutilisé après `b` — exige Copy
    }

    #[test]
    fn equality_is_by_value() {
        assert_eq!(RequestValueId(0), RequestValueId(0));
        assert_ne!(RequestValueId(0), RequestValueId(1));
    }
}

// ─── SegmentSelection — sélection AOT, référence jamais valeur (Phase 3,
//     GO 2026-09) ──────────────────────────────────────────────────────
//
// DESIGN §2.1. Le Core IR ne connaît JAMAIS de sémantique HTTP (pas de
// `PathParam("id")`, pas d'`IdSource` de registry.rs réimporté tel quel).
// Deux formes, aucune décision de représentation Rust arrêtée au-delà de
// ce qui suit — seule la PROPRIÉTÉ (référence AOT, jamais valeur runtime)
// est un invariant verrouillé :
//   - Constant(i64)   : valeur connue à la compilation (le cas Fixed(n) du
//                       routage actuel — reste une sélection, pas une
//                       absence de sélection).
//   - RequestSlot(..) : référence opaque à un slot du contexte de requête
//                       (le cas PathParam(name) du routage actuel — mais
//                       SANS le nom du paramètre, qui est une sémantique
//                       HTTP n'appartenant pas à ce crate).
//
// Fixed et PathParam (registry.rs::IdSource) ne sont PAS deux chemins
// architecturaux distincts au niveau de la résolution (DESIGN §2.1) : seule
// l'étape d'EXTRACTION de la valeur diffère (constante recopiée sans I/O
// pour l'une, lecture de paramètre d'URL pour l'autre) ; une fois cette
// valeur obtenue, les deux convergent vers exactement le même mécanisme de
// résolution physique (Phase 4) — aucune branche séparée ne doit apparaître
// à ce niveau, seulement, en amont, au niveau de l'obtention de la valeur.
//
// Volontairement PAS de #[repr(C)] à ce stade — même raisonnement que
// SourceSpec ci-dessus : aucune nécessité démontrée (ce type ne traverse
// aucune frontière FFI/mmap, n'entre dans aucune table binaire générée).
// Propriétés requises vérifiées par construction (Copy dérivé, aucun champ
// alloué) et par l'assertion needs_drop ci-dessous, plutôt que par une
// assertion de layout.
/// Référence AOT à la sélection d'un segment — jamais la valeur runtime
/// résolue pour une requête donnée (cf. DESIGN §2.1, distinction
/// sélection/valeur de sélection runtime).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentSelection {
    /// Valeur connue à la compilation — aucune extraction runtime requise.
    Constant(i64),
    /// Référence opaque vers un emplacement du contexte de requête —
    /// jamais une valeur HTTP elle-même.
    RequestSlot(RequestValueId),
}

const _: () = assert!(
    !std::mem::needs_drop::<SegmentSelection>(),
    "SegmentSelection ne doit jamais nécessiter de Drop"
);

#[cfg(test)]
mod tests_segment_selection {
    use super::{RequestValueId, SegmentSelection};

    #[test]
    fn is_copy_not_move() {
        let a = SegmentSelection::Constant(42);
        let b = a;
        assert_eq!(a, b);
        assert_eq!(a, SegmentSelection::Constant(42)); // `a` réutilisé après `b` — exige Copy
    }

    #[test]
    fn equality_is_structural_per_variant() {
        assert_eq!(SegmentSelection::Constant(1), SegmentSelection::Constant(1));
        assert_ne!(SegmentSelection::Constant(1), SegmentSelection::Constant(2));
        assert_eq!(
            SegmentSelection::RequestSlot(RequestValueId(0)),
            SegmentSelection::RequestSlot(RequestValueId(0))
        );
        assert_ne!(
            SegmentSelection::RequestSlot(RequestValueId(0)),
            SegmentSelection::RequestSlot(RequestValueId(1))
        );
    }

    #[test]
    fn different_variants_are_never_equal() {
        assert_ne!(
            SegmentSelection::Constant(0),
            SegmentSelection::RequestSlot(RequestValueId(0))
        );
    }

    #[test]
    fn never_needs_drop() {
        assert!(!std::mem::needs_drop::<SegmentSelection>());
    }
}

// ─── SegmentFlags — drapeaux d'émission d'un segment (Phase 3,
//     GO 2026-09) ──────────────────────────────────────────────────────
//
// DESIGN §2 : « ex: Volatile, réservé pour extension ». Un seul bit
// nécessaire à cette phase — VOLATILE — condition NÉCESSAIRE de la
// première clause de compatibilité SingleFile (DESIGN §9.1 : « aucun
// segment de la route n'est de variante Volatile »). Bits restants
// réservés, non nommés : les nommer par anticipation figerait un
// vocabulaire d'extension sans cas d'usage démontré.
//
// Représentation bit-à-bit à la main (pas de dépendance à la crate
// `bitflags`) : un seul bit à ce stade ne justifie pas une dépendance
// nouvelle du crate ; à reconsidérer si le nombre de drapeaux croît.
/// Drapeaux d'émission d'un segment — POD, `Copy`, sans sémantique au-delà
/// de ce que chaque bit documente explicitement.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentFlags(pub u8);

impl SegmentFlags {
    /// Aucun drapeau.
    pub const NONE: SegmentFlags = SegmentFlags(0);
    /// Le segment provient d'une Source volatile (`SourceSpec::VolatileSlot`).
    /// Condition nécessaire — DESIGN §9.1 — pour EXCLURE une route du
    /// backend `SingleFile` ; ne constitue pas à elle seule la condition
    /// suffisante (cf. rapport de session — second critère §9.1 différé).
    pub const VOLATILE: SegmentFlags = SegmentFlags(1 << 0);

    /// Test d'appartenance bit-à-bit — `const fn`, utilisable dans une
    /// future vérification à la compilation par un générateur AOT.
    #[inline(always)]
    pub const fn contains(self, flag: SegmentFlags) -> bool {
        self.0 & flag.0 == flag.0
    }

    #[inline(always)]
    pub const fn is_volatile(self) -> bool {
        self.contains(Self::VOLATILE)
    }
}

const _: () = assert!(
    std::mem::size_of::<SegmentFlags>() == std::mem::size_of::<u8>(),
    "SegmentFlags doit avoir exactement la taille de u8 (repr(transparent))"
);

#[cfg(test)]
mod tests_segment_flags {
    use super::SegmentFlags;

    #[test]
    fn none_contains_nothing() {
        assert!(!SegmentFlags::NONE.is_volatile());
        assert!(!SegmentFlags::NONE.contains(SegmentFlags::VOLATILE));
    }

    #[test]
    fn volatile_flag_is_detected() {
        assert!(SegmentFlags::VOLATILE.is_volatile());
        assert!(SegmentFlags::VOLATILE.contains(SegmentFlags::VOLATILE));
    }

    #[test]
    fn is_copy_not_move() {
        let a = SegmentFlags::VOLATILE;
        let b = a;
        assert_eq!(a, b);
        assert_eq!(a, SegmentFlags::VOLATILE); // `a` réutilisé après `b` — exige Copy
    }

    #[test]
    fn layout_is_exactly_one_byte() {
        assert_eq!(std::mem::size_of::<SegmentFlags>(), 1);
    }
}

// ─── SegmentDescriptor — IR produite par la Forge (Phase 3, GO 2026-09) ──
//
// DESIGN §2, corrigé post-confrontation au code réel : NE PORTE NI
// `offset` NI `len` (invalidé pour toute Source indexée — la majorité des
// routes réelles, cf. handlers.rs::serve_route/deliver, où (offset, len)
// provient de PackHtmlIndex::lookup() exécuté à chaque requête contre la
// génération actuellement publiée, jamais une constante figée à la
// compilation du binaire). `#[repr(C)]`/`Copy`/POD explicitement mandatés
// par le DESIGN (§2, « propriétés non négociables ») — à la différence de
// SourceSpec/SegmentSelection ci-dessus, cette exigence est ici déjà
// tranchée par le DESIGN, pas laissée à l'appréciation de cette phase.
/// Emplacement logique d'un morceau de la réponse HTTP — référence une
/// Source (`SourceId`), une sélection au sein de cette Source
/// (`SegmentSelection`) et des propriétés d'émission (`SegmentFlags`).
/// Ne contient JAMAIS de plage physique résolue (`offset`/`len`) — cf.
/// DESIGN §2/§3.1 (quatre cycles de validité).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentDescriptor {
    pub source: SourceId,
    pub selection: SegmentSelection,
    pub flags: SegmentFlags,
}

const _: () = assert!(
    !std::mem::needs_drop::<SegmentDescriptor>(),
    "SegmentDescriptor ne doit jamais nécessiter de Drop — chemin chaud \
     sans allocation (ADR-011 §7)"
);

#[cfg(test)]
mod tests_segment_descriptor {
    use super::{SegmentDescriptor, SegmentFlags, SegmentSelection, SourceId};

    fn sample(source: u16, selection: SegmentSelection, flags: SegmentFlags) -> SegmentDescriptor {
        SegmentDescriptor {
            source: SourceId(source),
            selection,
            flags,
        }
    }

    #[test]
    fn is_copy_not_move() {
        let a = sample(1, SegmentSelection::Constant(7), SegmentFlags::NONE);
        let b = a;
        assert_eq!(a, b);
        assert_eq!(
            a,
            sample(1, SegmentSelection::Constant(7), SegmentFlags::NONE)
        ); // `a` réutilisé après `b` — exige Copy
    }

    #[test]
    fn equality_is_structural() {
        assert_eq!(
            sample(1, SegmentSelection::Constant(1), SegmentFlags::NONE),
            sample(1, SegmentSelection::Constant(1), SegmentFlags::NONE)
        );
        assert_ne!(
            sample(1, SegmentSelection::Constant(1), SegmentFlags::NONE),
            sample(2, SegmentSelection::Constant(1), SegmentFlags::NONE)
        );
        assert_ne!(
            sample(1, SegmentSelection::Constant(1), SegmentFlags::NONE),
            sample(1, SegmentSelection::Constant(2), SegmentFlags::NONE)
        );
        assert_ne!(
            sample(1, SegmentSelection::Constant(1), SegmentFlags::NONE),
            sample(1, SegmentSelection::Constant(1), SegmentFlags::VOLATILE)
        );
    }

    #[test]
    fn never_needs_drop() {
        assert!(!std::mem::needs_drop::<SegmentDescriptor>());
    }

    // Table statique &'static [SegmentDescriptor] — usage attendu une fois
    // RouteDescriptor introduit (Phase 4), exercé ici au niveau le plus
    // simple possible pour vérifier que rien n'empêche cet usage.
    #[test]
    fn usable_in_a_static_slice() {
        static SEGMENTS: &[SegmentDescriptor] = &[
            SegmentDescriptor {
                source: SourceId(0),
                selection: SegmentSelection::Constant(1),
                flags: SegmentFlags::NONE,
            },
            SegmentDescriptor {
                source: SourceId(1),
                selection: SegmentSelection::RequestSlot(super::RequestValueId(0)),
                flags: SegmentFlags::VOLATILE,
            },
        ];
        assert_eq!(SEGMENTS.len(), 2);
        assert!(SEGMENTS[1].flags.is_volatile());
        assert!(!SEGMENTS[0].flags.is_volatile());
    }
}

// ─── EmissionBackendKind — décision de backend, par route (Phase 3,
//     GO 2026-09, complété post-amendement §9.1) ────────────────────────
//
// DESIGN §9 : décidé par la Forge, une fois par route — jamais recalculé
// au runtime (§9.2 : « le Request Context lit ce champ, il ne le déduit
// jamais »). Le prédicat `is_single_file_compatible` ci-dessous implémente
// le critère amendé de DESIGN §9.1 : `SingleFile` n'est certifié que
// lorsqu'il est AOT-prouvable avec les informations actuellement
// disponibles dans l'IR — jamais supposé. Il NE vérifie PAS et NE PEUT PAS
// vérifier de contiguïté physique entre segments : `SegmentDescriptor` ne
// porte aucune plage physique (§2), donc aucune information de
// contiguïté n'existe à ce niveau pour être inspectée, ici ou ailleurs
// dans l'IR AOT.
/// Backend d'émission consommant un `EmissionPlan` (DESIGN §9) — décidé
/// par la Forge, par route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmissionBackendKind {
    /// `sendfile(fd, offset, len)` — un seul descripteur de fichier, une
    /// seule plage. Certifié uniquement par `is_single_file_compatible`
    /// (DESIGN §9.1 amendé) — jamais déduit autrement.
    SingleFile,
    /// `writev`/`sendmsg` sur `IoSlice[]` — cas général, y compris toute
    /// route multi-segments même entièrement statique (DESIGN §9.1 amendé :
    /// aucune preuve AOT de contiguïté disponible aujourd'hui).
    Scatter,
}

/// Prédicat pur de compatibilité `SingleFile` — DESIGN §9.1 (amendé post-Phase
/// 3). Certifie *exactement* ce que l'IR actuel permet de prouver à la
/// compilation, ni plus ni moins :
///
/// - **`true`** si et seulement si la route ne comporte **qu'un seul**
///   `SegmentDescriptor`, non volatil ;
/// - **`false`** dans tous les autres cas, y compris plusieurs segments
///   statiques partageant la même Source — la contiguïté de leurs plages
///   n'est jamais une information disponible à ce niveau (§2, §3.1 : une
///   plage physique n'est connue qu'à la résolution runtime, jamais à la
///   compilation), donc jamais quelque chose que cette primitive pourrait
///   légitimement affirmer. Ceci est une condition **suffisante et
///   conservatrice**, pas la définition architecturale définitive de
///   `SingleFile` (DESIGN §9.1) : une route multi-segments réellement
///   contiguë existe peut-être, mais ce prédicat ne peut pas — et ne doit
///   pas prétendre — le savoir avec l'IR d'aujourd'hui.
///
/// **Précondition : `segments` non vide.** Une route sans aucun segment
/// est une erreur de génération AOT (DESIGN §9.1), jamais un cas
/// d'exécution valide que ce prédicat aurait à trancher entre `true` et
/// `false` — verrouillé par `debug_assert!` plutôt que par un `Result`
/// ou un panic inconditionnel : l'obligation de ne jamais atteindre ce
/// cas appartient au générateur de routes (Phase 4+), pas à cette
/// primitive pure, qui ne fait qu'exprimer l'hypothèse pour le
/// développement de ce générateur.
pub fn is_single_file_compatible(segments: &[SegmentDescriptor]) -> bool {
    debug_assert!(
        !segments.is_empty(),
        "is_single_file_compatible : route sans segment — erreur de \
         génération AOT (DESIGN §9.1), jamais un cas valide à cette étape"
    );
    match segments {
        [only] => !only.flags.is_volatile(),
        _ => false,
    }
}

#[cfg(test)]
mod tests_emission_backend_kind {
    use super::EmissionBackendKind;

    #[test]
    fn variants_are_distinct_and_copy() {
        let a = EmissionBackendKind::SingleFile;
        let b = a; // exige Copy
        assert_eq!(a, b);
        assert_ne!(
            EmissionBackendKind::SingleFile,
            EmissionBackendKind::Scatter
        );
    }
}

#[cfg(test)]
mod tests_is_single_file_compatible {
    use super::{
        SegmentDescriptor, SegmentFlags, SegmentSelection, SourceId, is_single_file_compatible,
    };

    fn seg(source: u16, flags: SegmentFlags) -> SegmentDescriptor {
        SegmentDescriptor {
            source: SourceId(source),
            selection: SegmentSelection::Constant(0),
            flags,
        }
    }

    #[test]
    fn single_non_volatile_segment_is_single_file() {
        let segments = [seg(0, SegmentFlags::NONE)];
        assert!(is_single_file_compatible(&segments));
    }

    #[test]
    fn single_volatile_segment_is_never_single_file() {
        let segments = [seg(0, SegmentFlags::VOLATILE)];
        assert!(!is_single_file_compatible(&segments));
    }

    #[test]
    fn two_static_segments_same_source_are_scatter_not_single_file() {
        // Même SourceId pour les deux — aucune contiguïté physique
        // prouvable AOT ne peut en être déduite (DESIGN §9.1 amendé) :
        // conservateur, donc Scatter, même si un observateur humain
        // pourrait soupçonner une contiguïté réelle au runtime.
        let segments = [seg(0, SegmentFlags::NONE), seg(0, SegmentFlags::NONE)];
        assert!(!is_single_file_compatible(&segments));
    }

    #[test]
    fn two_static_segments_different_sources_are_scatter() {
        let segments = [seg(0, SegmentFlags::NONE), seg(1, SegmentFlags::NONE)];
        assert!(!is_single_file_compatible(&segments));
    }

    #[test]
    fn multi_segment_route_with_one_volatile_is_scatter() {
        let segments = [seg(0, SegmentFlags::NONE), seg(1, SegmentFlags::VOLATILE)];
        assert!(!is_single_file_compatible(&segments));
    }

    #[test]
    #[should_panic(expected = "route sans segment")]
    #[cfg(debug_assertions)]
    fn empty_segments_violates_documented_precondition() {
        // N'exerce le debug_assert! que sous debug_assertions (comme tout
        // debug_assert!) — cf. commentaire de la primitive : le cas vide
        // est une erreur de GÉNÉRATION AOT à empêcher en amont (Phase
        // 4+), pas un cas que cette primitive doit gérer par une valeur
        // de retour arbitraire.
        let segments: [SegmentDescriptor; 0] = [];
        let _ = is_single_file_compatible(&segments);
    }
}

// ─── Budget HTTP de segments (K) et vérification IOV_MAX (Phase 3,

//     GO 2026-09) ──────────────────────────────────────────────────────
//
// DESIGN §7/§8 ADR-011, checkpoint §P — quatre budgets distincts, à ne
// jamais fusionner :
//   1. Projection::MAX_RENDER_CHUNKS (déjà en place, Phase 0.A) — budget
//      de rendu Forge, PAR ENREGISTREMENT, interne à UNE Projection.
//   2. SegmentBudget (ici) — nombre maximal de SegmentDescriptor composant
//      UNE ROUTE/réponse. Nouveau à cette phase.
//   3. SourceSpec::VolatileSlot.capacity (déjà en place, Phase 2) — borne
//      AOT de la production volatile, PAR SOURCE.
//   4. IOV_MAX/UIO_MAXIOV (ici) — plafond du système d'exploitation sur un
//      seul appel writev/sendmsg. Constante EXTERNE, pas un budget
//      architectural choisi par Marius.
// IOV_MAX ne remplace pas SegmentBudget — il le CONTRAINT (deux
// vérifications de nature différente, toutes deux nécessaires).
//
// SegmentBudget n'est porté par aucune structure existante à cette phase :
// son porteur naturel (RouteDescriptor.segments: &'static [SegmentDescriptor],
// dont SegmentBudget serait la longueur) est Phase 4 — hors périmètre ici.
// Ce type donne un nom stable au concept avant que son porteur concret
// existe, sans préjuger de la forme de ce porteur.
/// Nombre maximal de `SegmentDescriptor` composant une route — DESIGN
/// §7/§8 ADR-011. Distinct de `Projection::MAX_RENDER_CHUNKS` et de
/// `SourceSpec::VolatileSlot::capacity` (voir le commentaire de section
/// ci-dessus).
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct SegmentBudget(pub usize);

/// Limite IOV_MAX (`UIO_MAXIOV`) du build Linux actuel — **valeur de
/// plateforme, pas une propriété universelle du Core** (cf. DESIGN §7 :
/// distinction entre la règle architecturale, la valeur retenue pour le
/// build actuel, et le lieu où cette valeur sera injectée/vérifiée une
/// fois le générateur AOT existant — Phase 4+, non tranché ici). Cette
/// constante représente uniquement le deuxième terme de cette distinction
/// pour la plateforme ciblée aujourd'hui.
pub const IOV_MAX_CURRENT_PLATFORM: usize = 1024;

/// Règle architecturale pure (DESIGN §7 : « IOV_MAX ne remplace pas K — il
/// le contraint ») : un `SegmentBudget` ne doit jamais dépasser la limite
/// d'I/O vectoriel applicable. Générique sur `iov_limit` — ne présuppose
/// pas `IOV_MAX_CURRENT_PLATFORM`, pour que la règle reste valide
/// indépendamment de la plateforme de build qui l'invoquera. `const fn` :
/// utilisable dans une future assertion à la compilation par un
/// générateur AOT (`const _: () = assert!(segment_budget_fits_iov_limit(...))`),
/// sans que cette phase ne décide QUI émet cette assertion ni DEPUIS QUEL
/// crate (différé — cf. rapport de session).
#[inline(always)]
pub const fn segment_budget_fits_iov_limit(budget: SegmentBudget, iov_limit: usize) -> bool {
    budget.0 <= iov_limit
}

#[cfg(test)]
mod tests_segment_budget_and_iov {
    use super::{IOV_MAX_CURRENT_PLATFORM, SegmentBudget, segment_budget_fits_iov_limit};

    #[test]
    fn budget_within_limit_passes() {
        assert!(segment_budget_fits_iov_limit(SegmentBudget(3), 1024));
    }

    #[test]
    fn budget_at_exact_limit_passes() {
        assert!(segment_budget_fits_iov_limit(SegmentBudget(1024), 1024));
    }

    #[test]
    fn budget_exceeding_limit_fails() {
        assert!(!segment_budget_fits_iov_limit(SegmentBudget(1025), 1024));
    }

    #[test]
    fn rule_is_generic_over_the_supplied_limit_not_hardcoded() {
        // La règle ne doit pas être câblée sur IOV_MAX_CURRENT_PLATFORM —
        // elle doit rester valide pour toute limite reçue en paramètre.
        assert!(segment_budget_fits_iov_limit(SegmentBudget(2048), 4096));
        assert!(!segment_budget_fits_iov_limit(SegmentBudget(2048), 1024));
    }

    #[test]
    fn current_platform_constant_matches_documented_linux_value() {
        assert_eq!(IOV_MAX_CURRENT_PLATFORM, 1024);
    }

    #[test]
    fn const_evaluable_at_compile_time() {
        const _: () = assert!(segment_budget_fits_iov_limit(SegmentBudget(64), 1024));
    }
}

pub type BatchResult<P> =
    Result<Vec<(<P as Projection>::Record, <P as Projection>::VarlenOwned)>, sqlx::Error>;

/// Un fragment ordonné du résultat d'un rendu — CONTRAT-implementation-
/// projection-segmentee.md, Étape 2 (corrigé en session, 23/07/2026 : ce type
/// vit dans `marius_projection`, pas dans `marius_fragment_forge` — c'est ici
/// que le trait `Projection` le consomme, et `marius_render`/le crate généré
/// dépendent déjà de ce crate ; `fragment-forge` est un outil de build-time,
/// jamais une dépendance runtime).
///
/// Généralise au-delà du cas des varlena volumineux (ADR-010 §7) : `RenderChunk`
/// ne code en dur aucune notion de « gros champ HTML » — un composant sans
/// champ `marius:large_content` produit toujours un unique `RenderChunk::Buffered`
/// couvrant tout `buf` (implémentation par défaut de
/// `Projection::render_chunks` ci-dessous), sans changement de comportement.
///
/// Pourquoi `Buffered { start, end }` et non `Buffered(&'a str)` (arbitré en
/// session, 23/07/2026) : `render_chunks` continue d'écrire dans `buf`
/// après avoir logiquement « produit » un premier segment (ex. en-tête déjà
/// écrit, pied écrit plus tard dans le même appel). Un `&'a str` emprunté sur
/// `buf` et conservé dans la séquence de segments retournée maintiendrait un
/// prêt immuable vivant pendant que la fonction continue de faire
/// `buf.push_str(...)` pour la suite — prêt immuable et mutation simultanés
/// sur le même `buf`, rejeté par le borrow checker à raison : `String` peut
/// réallouer, ce qui invaliderait toute `&str` prise avant la dernière
/// écriture. Les indices diffèrent la vue en `&str` jusqu'à ce que `buf` soit
/// stable — après le retour de `render_chunks`, quand l'appelant (qui
/// possède déjà `buf` dans son intégralité) peut re-trancher
/// `&buf[start..end]` sans risque. Ce n'est pas une fuite de représentation
/// interne : l'appelant ne gagne aucune information qu'il ne pourrait déjà
/// déduire, puisqu'il possède `buf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderChunk<'a> {
    /// Plage déjà écrite dans le buffer partagé réutilisé (`buf[start..end]`).
    /// Jamais valide après que `buf` a été vidé/réutilisé pour l'enregistrement
    /// suivant — à consommer avant le prochain appel à `render_chunks`.
    Buffered { start: usize, end: usize },
    /// Référence empruntée, zéro copie — jamais recopiée dans `buf`. Portée
    /// par la donnée déjà possédée du composant (`VarlenOwned`), jamais par
    /// `buf` lui-même.
    Borrowed(&'a str),
}

#[cfg(test)]
mod tests_segment {
    use super::RenderChunk;

    #[test]
    fn buffered_variants_compare_by_value() {
        let a = RenderChunk::Buffered { start: 0, end: 10 };
        let b = RenderChunk::Buffered { start: 0, end: 10 };
        let c = RenderChunk::Buffered { start: 0, end: 11 };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn borrowed_variants_compare_by_value() {
        let a = RenderChunk::Borrowed("abc");
        let b = RenderChunk::Borrowed("abc");
        let c = RenderChunk::Borrowed("abcd");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn buffered_and_borrowed_are_never_equal() {
        let a = RenderChunk::Buffered { start: 0, end: 3 };
        let b = RenderChunk::Borrowed("abc");
        assert_ne!(a, b);
    }
}

pub trait Projection: Sized + Send + Sync + 'static {
    type Record: Sized + Send + 'static;
    type VarlenOwned: Sized + Send + 'static;

    // ── Voie d'Extraction (cold path — marius-dump) ───────────────────────────
    //
    // Accès PostgreSQL direct via SQLx. Allocations autorisées.
    // Appelée exclusivement par dumper::dump_table pour peupler le store.bin.
    // Default : retourne Err — la Forge génère l'override pour chaque Projection.
    fn fetch_from_pg(
        _pool: &sqlx::PgPool,
        _ids: &[i64],
    ) -> impl std::future::Future<Output = BatchResult<Self>> + Send {
        std::future::ready(Err(sqlx::Error::Configuration(
            "[fetch_from_pg] override SQLx non généré — exécuter cargo build".into(),
        )))
    }

    // ── Voie d'Exécution (hot path — serveur) ────────────────────────────────
    //
    // Lecture mmap via OnceLock<PackfileReader>. Zéro allocation.
    // Fail-fast si store.bin absent : exécuter marius-dump d'abord.
    fn fetch_batch(
        pool: &sqlx::PgPool,
        ids: &[i64],
    ) -> impl std::future::Future<Output = BatchResult<Self>> + Send;

    fn render(record: &Self::Record, varlena: &Self::VarlenOwned, buf: &mut String);

    /// Nombre maximal de segments produits par un enregistrement de ce
    /// composant — CONTRAT-implementation-projection-segmentee.md, Étape 3.
    /// Connu statiquement, généré par db-forge/fragment-forge selon le
    /// template compilé (propriété du template, exprimée ici via le
    /// mécanisme de surcharge du trait — `impl Projection for {Name}Projection`
    /// est lui-même entièrement généré). Défaut `1` : un composant sans champ
    /// `marius:large_content` produit toujours exactement un segment.
    /// Permet à `BatchRenderer` de pré-allouer son `Vec<RenderChunk>` une seule
    /// fois, jamais de resize en boucle de rendu (même discipline que
    /// `buf`/`total_cap`, INV-5/INV-6 de `PackfileBuilder`).
    const MAX_RENDER_CHUNKS: usize = 1;

    /// Par défaut, délègue à `render()` — un seul segment `Buffered` couvrant
    /// tout `buf`. Composants sans champ `marius:large_content` : comportement
    /// inchangé, coût additionnel négligeable (un `push()` dans un `Vec`
    /// pré-alloué à `MAX_RENDER_CHUNKS`).
    ///
    /// Les composants générés avec un champ `marius:large_content` reçoivent
    /// une implémentation réelle multi-segments (générée par
    /// `fragment-forge`/`db-forge`, Étape 5 du Contrat) qui ne délègue jamais
    /// à cette valeur par défaut — le champ volumineux y devient un
    /// `RenderChunk::Borrowed` autonome, jamais concaténé dans `buf`.
    ///
    /// **Contrat sur `buf` (précisé en session, 23/07/2026)** : `buf` arrive
    /// déjà vide — le nettoyage est la responsabilité exclusive de
    /// l'appelant (`BatchRenderer::render_batch`), jamais de cette méthode ni
    /// de `render()`, exactement comme aujourd'hui pour `render()` seul. Ce
    /// n'est pas cosmétique : une implémentation multi-segments doit pouvoir
    /// écrire l'en-tête dans `buf`, laisser `buf` intact pendant qu'un segment
    /// emprunté est produit, puis **continuer à écrire** le pied à la suite
    /// dans le même `buf` sans le vider entre-temps — sans quoi le premier
    /// `RenderChunk::Buffered` référencerait des octets déjà écrasés. Un
    /// `buf.clear()` interne à cette méthode casserait ce cas pour toute
    /// implémentation réelle multi-segments.
    ///
    /// `render()` reste la seule méthode que `render_chunks` appelle pour
    /// produire du contenu dans le cas par défaut — cette méthode ne connaît
    /// toujours que `&mut String`, jamais `Write`/socket/fichier (invariant
    /// préservé, cf. ADR-010 §3).
    fn render_chunks<'a>(
        record: &Self::Record,
        varlena: &'a Self::VarlenOwned,
        buf: &mut String,
        segments: &mut Vec<RenderChunk<'a>>,
    ) {
        Self::render(record, varlena, buf);
        segments.push(RenderChunk::Buffered {
            start: 0,
            end: buf.len(),
        });
    }

    fn record_id(record: &Self::Record) -> i64;

    fn packfile_path() -> PathBuf;

    fn store_path() -> PathBuf;

    /// Accès générique au registre atomiquement remplaçable de cette
    /// Projection — nécessaire à tout code générique `<P: Projection>`
    /// (`ingest_and_swap`) qui doit appeler `.swap()` sans connaître la
    /// `static` propre à P, invisible depuis une fonction générique.
    /// `cold_start_store()` (généré, méthode inhérente hors trait) et cette
    /// méthode ciblent la même `static` — cf. codegen/projection.rs.
    fn store_registry() -> &'static StoreRegistry<Self>
    where
        Self: Sized,
        Self::Record: bytemuck::Pod;

    #[inline(always)]
    fn varlena_field_count() -> u16 {
        0
    }

    #[inline(always)]
    fn encode_varlena(
        _varlena: &Self::VarlenOwned,
        _heap: &mut Vec<u8>,
        _toc: &mut Vec<VarlenSlot>,
    ) {
    }
}

#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct VarlenSlot {
    pub offset: u32,
    pub len: u32,
}

// =============================================================================
// Protocole binaire — source de vérité unique
//
// PackfileStoreHeader et align8 sont définis ici et importés par :
//   - marius_render::packfile_builder (écriture)
//   - marius_projection::packfile_reader (lecture)
//
// Toute modification de layout est répercutée sur les deux côtés sans risque
// de dérive silencieuse.
// =============================================================================

/// Header du store.bin — exactement 64B (une cache line).
/// Placé en tête du fichier, lu au montage par PackfileReader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PackfileStoreHeader {
    pub magic: [u8; 8], // b"MARIUSDB"
    pub version: u32,   // = 1
    pub stride: u32,    // sizeof(P::Record)
    pub row_count: u64,
    pub varlena_field_count: u16,
    pub _pad: [u8; 6],
    pub id_index_section: u64,
    pub varlena_toc_section: u64,
    pub varlena_heap_section: u64,
    pub varlena_heap_len: u64,
}

const _: () = assert!(
    std::mem::size_of::<PackfileStoreHeader>() == 64,
    "PackfileStoreHeader doit être exactement 64B"
);

/// Arrondit `x` au prochain multiple de 8.
/// Utilisé par Builder (écriture des sections) et Reader (validation des offsets).
#[inline(always)]
pub const fn align8(x: u64) -> u64 {
    (x + 7) & !7
}

// =============================================================================
// PackfileReader — lecteur zero-copie via memmap2
// =============================================================================

mod store_registry;
pub use store_registry::StoreRegistry;

pub mod packfile_reader {
    use std::fs::File;
    use std::marker::PhantomData;
    use std::mem;
    use std::path::Path;

    use bytemuck::Pod;
    use memmap2::Mmap;

    use super::{PackfileStoreHeader, Projection, VarlenSlot};

    /// Vue sur les champs varlena d'un enregistrement.
    /// Zéro copie — lifetime lié au PackfileReader.
    pub struct VarlenRefs<'a> {
        toc: &'a [VarlenSlot],
        heap: &'a [u8],
    }

    impl<'a> VarlenRefs<'a> {
        /// Accès par index (0-based, ordre attnum).
        /// None si sentinel (offset == u32::MAX) ou index hors bornes.
        #[inline(always)]
        pub fn get(&self, field_idx: usize) -> Option<&'a str> {
            let slot = self.toc.get(field_idx)?;
            if slot.offset == u32::MAX {
                return None;
            }
            let start = slot.offset as usize;
            let end = start + slot.len as usize;
            std::str::from_utf8(self.heap.get(start..end)?).ok()
        }
    }

    /// Lecteur zero-copie d'un store.bin produit par PackfileBuilder<P>.
    ///
    /// Conçu pour être stocké dans un OnceLock<PackfileReader<P>> statique.
    /// memmap2::Mmap est Send + Sync.
    pub struct PackfileReader<P: Projection>
    where
        P::Record: Pod,
    {
        mmap: Mmap,
        row_count: usize,
        varlena_field_count: usize,
        rows_offset: usize,
        id_index_offset: usize,
        toc_offset: usize,
        heap_offset: usize,
        heap_len: usize,
        _proj: PhantomData<P>,
    }

    impl<P: Projection> PackfileReader<P>
    where
        P::Record: Pod,
    {
        /// Ouvre `path`, le mappe en lecture seule, valide le header.
        /// Appelle madvise(MADV_WILLNEED) pour pré-charger les pages en RAM
        /// dès le montage — élimine les page faults en hot path Tokio.
        ///
        /// # Safety (mmap)
        /// store.bin est produit atomiquement par marius-dump (INV-6).
        /// Il n'est pas modifié pendant l'exécution du serveur.
        pub fn open(path: &Path) -> std::io::Result<Self> {
            let file = File::open(path)?;
            let mmap = unsafe { Mmap::map(&file)? };

            // Pré-chargement des pages — hint non bloquant, sans privilège requis.
            // Élimine les page faults lors des premiers lookups en hot path.
            let _ = mmap.advise(memmap2::Advice::WillNeed);

            let header_size = mem::size_of::<PackfileStoreHeader>();

            if mmap.len() < header_size {
                return Err(std::io::Error::other(format!(
                    "[PackfileReader] fichier trop court : {}B < {}B",
                    mmap.len(),
                    header_size,
                )));
            }

            let header: &PackfileStoreHeader = bytemuck::from_bytes(&mmap[..header_size]);

            if &header.magic != b"MARIUSDB" {
                return Err(std::io::Error::other(format!(
                    "[PackfileReader] magic invalide : {:?}",
                    header.magic,
                )));
            }
            if header.version != 1 {
                return Err(std::io::Error::other(format!(
                    "[PackfileReader] version non supportée : {}",
                    header.version,
                )));
            }

            let expected_stride = mem::size_of::<P::Record>() as u32;
            if header.stride != expected_stride {
                return Err(std::io::Error::other(format!(
                    "[PackfileReader] stride incohérent : header={}B, sizeof(Record)={}B",
                    header.stride, expected_stride,
                )));
            }

            let expected_len = (header.varlena_heap_section + header.varlena_heap_len) as usize;
            if mmap.len() != expected_len {
                return Err(std::io::Error::other(format!(
                    "[PackfileReader] taille incohérente : {}B != {}B (header)",
                    mmap.len(),
                    expected_len,
                )));
            }

            Ok(Self {
                row_count: header.row_count as usize,
                varlena_field_count: header.varlena_field_count as usize,
                rows_offset: header_size,
                id_index_offset: header.id_index_section as usize,
                toc_offset: header.varlena_toc_section as usize,
                heap_offset: header.varlena_heap_section as usize,
                heap_len: header.varlena_heap_len as usize,
                mmap,
                _proj: PhantomData,
            })
        }

        /// Tranche brute des enregistrements, triée par position (pas par id).
        /// Exposé pour merge_store (marius-render) — memcpy de runs, zéro copie.
        #[inline(always)]
        pub fn records(&self) -> &[P::Record] {
            let end = self.rows_offset + self.row_count * mem::size_of::<P::Record>();
            bytemuck::cast_slice(&self.mmap[self.rows_offset..end])
        }

        /// Index des ids, trié croissant — invariant déjà exploité par `lookup`.
        #[inline(always)]
        pub fn id_index(&self) -> &[i64] {
            let end = self.id_index_offset + self.row_count * mem::size_of::<i64>();
            bytemuck::cast_slice(&self.mmap[self.id_index_offset..end])
        }

        /// TOC varlena brut, `row_count * varlena_field_count` entrées.
        #[inline(always)]
        pub fn toc(&self) -> &[VarlenSlot] {
            let len = self.row_count * self.varlena_field_count * mem::size_of::<VarlenSlot>();
            bytemuck::cast_slice(&self.mmap[self.toc_offset..self.toc_offset + len])
        }

        /// Heap varlena brut, tassé — les offsets du TOC y pointent directement.
        #[inline(always)]
        pub fn heap(&self) -> &[u8] {
            &self.mmap[self.heap_offset..self.heap_offset + self.heap_len]
        }

        /// Nombre de champs varlena par ligne — nécessaire à l'appelant pour
        /// calculer les bornes d'un slice `toc()` par plage de lignes.
        #[inline(always)]
        pub fn varlena_field_count(&self) -> usize {
            self.varlena_field_count
        }

        /// Recherche par ID — O(log N) binary search.
        /// Zéro allocation — toutes les références pointent dans le mmap.
        #[inline]
        pub fn lookup(&self, id: i64) -> Option<(&P::Record, VarlenRefs<'_>)> {
            let pos = self.id_index().binary_search(&id).ok()?;
            let record = &self.records()[pos];
            let toc_all = self.toc();
            let heap = self.heap();

            let toc_base = pos * self.varlena_field_count;
            let toc_slice = &toc_all[toc_base..toc_base + self.varlena_field_count];

            Some((
                record,
                VarlenRefs {
                    toc: toc_slice,
                    heap,
                },
            ))
        }

        #[inline(always)]
        pub fn row_count(&self) -> usize {
            self.row_count
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::VarlenSlot;

        #[test]
        fn sentinel_returns_none() {
            let slots = [VarlenSlot {
                offset: u32::MAX,
                len: 0,
            }];
            let refs = VarlenRefs {
                toc: &slots,
                heap: &[],
            };
            assert_eq!(refs.get(0), None);
        }

        #[test]
        fn valid_slot_returns_str() {
            let slots = [VarlenSlot { offset: 0, len: 5 }];
            let refs = VarlenRefs {
                toc: &slots,
                heap: b"hello",
            };
            assert_eq!(refs.get(0), Some("hello"));
        }

        #[test]
        fn out_of_bounds_field_returns_none() {
            let slots = [VarlenSlot { offset: 0, len: 2 }];
            let refs = VarlenRefs {
                toc: &slots,
                heap: b"hi",
            };
            assert_eq!(refs.get(1), None);
        }
    }
}
