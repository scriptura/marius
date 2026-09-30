// crates/shell/render/src/volatile_producers.rs

//! Producteur Volatile applicatif — V3a (vertical slice `nav_profile`).
//!
//! ```text
//! contexte runtime (VolatileContext, possédé)
//!      ↓
//! ProducerKey(0) → produce_nav_profile
//!      ↓
//! VolatileStorage (capacité Forge, effective_len ≤ capacity)
//!      ↓
//! MaterializedSource::Volatile → resolve_volatile_range → ResolvedRange
//! ```
//!
//! ## Ce que ce module est — et n'est pas
//!
//! - UN producteur explicite (`nav_profile`), sélectionné par un `match`
//!   sur la `ProducerKey` de la Source (P8) — pas un registre, pas un trait
//!   de producteur : `ProducerKey(0)` reste la valeur provisoire du
//!   vertical slice (V2c). Un second producteur ajouterait un bras à ce
//!   `match`, jamais une abstraction.
//! - Transport-agnostique (comme le reste de `marius-render`) : aucune
//!   dépendance `axum`/`hyper`/`bytes`. L'adaptation `Bytes::from_owner`
//!   et le montage HTTP restent V3b.
//! - Respecte sans modification le contrat Volatile V1 (P1–P8) : le
//!   contenu est produit dans un `Vec<u8>` possédé puis pris en charge par
//!   `VolatileStorage::from_produced` (aucune copie, aucun raw pointer) ;
//!   `capacity` vient de la Source (Forge) ; un dépassement est une erreur
//!   contrôlée, jamais une troncature.
//!
//! ## Snapshot / pas de référence à travers un `await`
//!
//! `VolatileContext` est POSSÉDÉ (`Option<String>`), jamais un emprunt sur
//! la requête. `materialize_volatile` est synchrone : le contexte n'est lu
//! que pendant l'appel, le résultat est un `MaterializedSource` possédé
//! (`Arc<VolatileStorage>`) — l'appelant (V3b) peut donc le conserver
//! au-delà de tout `await` sans qu'aucune référence sur le contexte ou la
//! requête ne le traverse.

use marius_projection::{ProducerKey, SourceSpec};

use crate::emission::{
    MaterializedSource, VolatileCapacityExceeded, resolve_volatile_generation,
};

/// `ProducerKey` du producteur `nav_profile` — valeur provisoire du
/// vertical slice, celle que la Forge génère dans `ROUTE_DESCRIPTORS`
/// (`ProducerKey(0)`, V2c) ; jamais dérivée de la position d'un artefact.
pub const NAV_PROFILE_PRODUCER: ProducerKey = ProducerKey(0);

/// Contexte runtime consommé par les producteurs — possédé, cloneable, sans
/// lifetime (cf. « snapshot » en tête de module). Aujourd'hui : le seul
/// champ dont `nav_profile` a besoin. Alimenté par V3b (requête/session) ;
/// V3a ne fait aucune hypothèse sur cette provenance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolatileContext {
    /// Nom d'utilisateur connecté, `None` si anonyme.
    pub username: Option<String>,
}

impl VolatileContext {
    pub fn anonymous() -> Self {
        Self { username: None }
    }

    pub fn with_username(username: impl Into<String>) -> Self {
        Self {
            username: Some(username.into()),
        }
    }
}

/// Échecs contrôlés de la production volatile — jamais un panic. La
/// traduction en réponse HTTP 500 reste à la charge de l'adaptateur (V3b),
/// conformément au contrat P2/P7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolatileProductionError {
    /// La Source n'est pas un `VolatileSlot` (incohérence P7 côté appelant).
    NotAVolatileSlot,
    /// Aucun producteur connu pour cette `ProducerKey`.
    UnknownProducer(ProducerKey),
    /// `effective_len > capacity` (P2) — aucune troncature.
    CapacityExceeded(VolatileCapacityExceeded),
}

/// Échappe le texte destiné à un nœud HTML (et à une valeur d'attribut) :
/// `& < > " '`. Le username est une donnée utilisateur — jamais injecté brut.
fn escape_html_into(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
}

const NAV_PROFILE_OPEN: &str = "<li class=\"nav-profile\">";
const NAV_PROFILE_CLOSE: &str = "</li>";

/// Producteur `nav_profile` : `<li class="nav-profile">{username échappé}</li>`.
/// Anonyme → `<li class="nav-profile"></li>`, octet pour octet le
/// placeholder AOT de `navigation.marius` (une page sans utilisateur reste
/// identique au rendu monolithique).
///
/// Retourne un `Vec<u8>` possédé, dimensionné au contenu : pris tel quel par
/// `VolatileStorage::from_produced` (String → Vec<u8> sans copie).
pub fn produce_nav_profile(ctx: &VolatileContext) -> Vec<u8> {
    let username = ctx.username.as_deref().unwrap_or("");
    let mut html = String::with_capacity(
        NAV_PROFILE_OPEN.len() + username.len() + NAV_PROFILE_CLOSE.len(),
    );
    html.push_str(NAV_PROFILE_OPEN);
    escape_html_into(username, &mut html);
    html.push_str(NAV_PROFILE_CLOSE);
    html.into_bytes()
}

/// Matérialise une Source `VolatileSlot` : sélectionne le producteur par sa
/// `ProducerKey`, le fait produire à partir de `ctx`, et confie le contenu à
/// `resolve_volatile_generation` (`VolatileStorage`, vérification de
/// capacité). Point d'entrée unique de V3b pour un segment volatile.
///
/// Synchrone par construction (cf. « snapshot » en tête de module).
pub fn materialize_volatile(
    spec: &SourceSpec,
    ctx: &VolatileContext,
) -> Result<MaterializedSource, VolatileProductionError> {
    let SourceSpec::VolatileSlot { producer, .. } = spec else {
        return Err(VolatileProductionError::NotAVolatileSlot);
    };

    let produce: fn(&VolatileContext) -> Vec<u8> = match *producer {
        NAV_PROFILE_PRODUCER => produce_nav_profile,
        other => return Err(VolatileProductionError::UnknownProducer(other)),
    };

    match resolve_volatile_generation(spec, |_key| produce(ctx)) {
        Some(Ok(source)) => Ok(source),
        Some(Err(exceeded)) => Err(VolatileProductionError::CapacityExceeded(exceeded)),
        // Non atteignable : `spec` est un VolatileSlot (vérifié ci-dessus).
        None => Err(VolatileProductionError::NotAVolatileSlot),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emission::{resolve_volatile_range, source_spec_for};
    use marius_projection::SourceKey;
    use std::sync::Arc;

    fn nav_profile_spec(capacity: u32) -> SourceSpec {
        SourceSpec::VolatileSlot {
            capacity,
            producer: NAV_PROFILE_PRODUCER,
        }
    }

    fn resolved_bytes(source: &MaterializedSource) -> Vec<u8> {
        resolve_volatile_range(source)
            .expect("Volatile doit se résoudre")
            .as_slice()
            .to_vec()
    }

    // ── ProducerKey(0) → HTML → VolatileStorage → résolution ────────────

    #[test]
    fn producer_zero_renders_the_username_into_the_nav_profile_li() {
        let source = materialize_volatile(&nav_profile_spec(512), &VolatileContext::with_username("Alice"))
            .expect("doit réussir");
        assert_eq!(
            resolved_bytes(&source),
            b"<li class=\"nav-profile\">Alice</li>".to_vec()
        );
    }

    #[test]
    fn effective_len_is_the_produced_length_not_the_forge_capacity() {
        let source = materialize_volatile(&nav_profile_spec(512), &VolatileContext::with_username("Alice"))
            .unwrap();
        let MaterializedSource::Volatile { storage } = &source else {
            panic!("attendu Volatile");
        };
        let expected = b"<li class=\"nav-profile\">Alice</li>".len();
        assert_eq!(storage.effective_len(), expected);
        assert_eq!(storage.capacity(), 512);
        assert_eq!(resolve_volatile_range(&source).unwrap().len(), expected);
    }

    #[test]
    fn anonymous_context_equals_the_aot_placeholder_of_navigation_marius() {
        let source =
            materialize_volatile(&nav_profile_spec(512), &VolatileContext::anonymous()).unwrap();
        assert_eq!(
            resolved_bytes(&source),
            b"<li class=\"nav-profile\"></li>".to_vec()
        );
    }

    #[test]
    fn username_is_html_escaped_never_injected_raw() {
        let hostile = "<script>alert(\"x\")</script>&'";
        let source =
            materialize_volatile(&nav_profile_spec(512), &VolatileContext::with_username(hostile))
                .unwrap();
        let html = String::from_utf8(resolved_bytes(&source)).unwrap();
        assert_eq!(
            html,
            "<li class=\"nav-profile\">&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;&amp;&#39;</li>"
        );
        assert!(!html.contains("<script>"));
    }

    // ── ownership / snapshot (invariants V1 conservés) ──────────────────

    #[test]
    fn materialized_snapshot_survives_context_drop_and_later_productions() {
        let first = {
            let ctx = VolatileContext::with_username("Alice");
            materialize_volatile(&nav_profile_spec(512), &ctx).unwrap()
            // `ctx` est droppé ici : aucune référence sur lui dans `first`.
        };

        // Production ultérieure, contexte différent : n'affecte pas `first`.
        let second =
            materialize_volatile(&nav_profile_spec(512), &VolatileContext::with_username("Bob"))
                .unwrap();

        assert_eq!(
            resolved_bytes(&first),
            b"<li class=\"nav-profile\">Alice</li>".to_vec()
        );
        assert_eq!(
            resolved_bytes(&second),
            b"<li class=\"nav-profile\">Bob</li>".to_vec()
        );
    }

    #[test]
    fn storage_outlives_the_materialized_source_through_a_cloned_handle() {
        let source =
            materialize_volatile(&nav_profile_spec(512), &VolatileContext::with_username("Alice"))
                .unwrap();
        let MaterializedSource::Volatile { storage } = &source else {
            panic!("attendu Volatile");
        };
        let handle = Arc::clone(storage); // ce que fera l'adaptateur (P4)
        drop(source);
        assert_eq!(handle.as_slice(), b"<li class=\"nav-profile\">Alice</li>");
    }

    #[test]
    fn resolved_range_points_into_the_owned_buffer_without_copy() {
        let source =
            materialize_volatile(&nav_profile_spec(512), &VolatileContext::with_username("Alice"))
                .unwrap();
        let MaterializedSource::Volatile { storage } = &source else {
            panic!("attendu Volatile");
        };
        let range = resolve_volatile_range(&source).unwrap();
        assert_eq!(range.ptr(), storage.as_slice().as_ptr());
    }

    // ── overflow : erreur contrôlée, aucune troncature ──────────────────

    #[test]
    fn capacity_overflow_is_a_controlled_error_not_a_truncation() {
        let long_name = "x".repeat(64);
        let result = materialize_volatile(
            &nav_profile_spec(32),
            &VolatileContext::with_username(long_name.as_str()),
        );
        let expected_len = NAV_PROFILE_OPEN.len() + 64 + NAV_PROFILE_CLOSE.len();
        match result {
            Err(VolatileProductionError::CapacityExceeded(e)) => {
                assert_eq!(e.capacity, 32);
                assert_eq!(e.effective_len, expected_len);
            }
            Err(other) => panic!("mauvaise erreur : {other:?}"),
            Ok(_) => panic!("effective_len > capacity ne doit jamais réussir"),
        }
    }

    #[test]
    fn escaping_counts_toward_capacity() {
        // 20 chevrons → 80 octets échappés (&lt;), largement > 32.
        let result = materialize_volatile(
            &nav_profile_spec(32),
            &VolatileContext::with_username("<".repeat(20)),
        );
        assert!(matches!(
            result,
            Err(VolatileProductionError::CapacityExceeded(_))
        ));
    }

    #[test]
    fn exact_capacity_is_accepted() {
        let exact = NAV_PROFILE_OPEN.len() + NAV_PROFILE_CLOSE.len(); // anonyme
        assert!(materialize_volatile(&nav_profile_spec(exact as u32), &VolatileContext::anonymous()).is_ok());
        assert!(matches!(
            materialize_volatile(&nav_profile_spec(exact as u32 - 1), &VolatileContext::anonymous()),
            Err(VolatileProductionError::CapacityExceeded(_))
        ));
    }

    // ── incohérences P7/P8 : erreurs contrôlées, jamais de panic ────────

    #[test]
    fn unknown_producer_key_is_a_controlled_error() {
        let spec = SourceSpec::VolatileSlot {
            capacity: 512,
            producer: ProducerKey(9),
        };
        assert!(matches!(
            materialize_volatile(&spec, &VolatileContext::anonymous()),
            Err(VolatileProductionError::UnknownProducer(ProducerKey(9)))
        ));
    }

    #[test]
    fn non_volatile_source_is_a_controlled_error() {
        let spec = SourceSpec::StaticArtifact { key: SourceKey(0) };
        assert!(matches!(
            materialize_volatile(&spec, &VolatileContext::anonymous()),
            Err(VolatileProductionError::NotAVolatileSlot)
        ));
    }

    // ── liaison avec la sortie réelle de la Forge (V2c) ─────────────────

    /// Le slot volatile réellement généré (`ROUTE_DESCRIPTORS`, capacité et
    /// `ProducerKey` fournis par publication.toml) est matérialisable par ce
    /// producteur — c'est ce que V3b consommera tel quel.
    #[test]
    fn forge_generated_volatile_slot_is_materialized_by_producer_zero() {
        let index = marius_schema::ROUTES
            .iter()
            .position(|r| r.name == "content_document")
            .expect("route content_document");
        let descriptor = marius_schema::ROUTE_DESCRIPTORS[index];

        let volatile_segment = descriptor.segments[1];
        assert!(marius_projection::segment_matches_source(
            &volatile_segment,
            source_spec_for(&descriptor, volatile_segment.source).unwrap()
        ));

        let spec = source_spec_for(&descriptor, volatile_segment.source).expect("source du slot");
        let source = materialize_volatile(spec, &VolatileContext::with_username("Alice"))
            .expect("le slot généré par la Forge doit être produisible");
        assert_eq!(
            resolved_bytes(&source),
            b"<li class=\"nav-profile\">Alice</li>".to_vec()
        );
    }
}
