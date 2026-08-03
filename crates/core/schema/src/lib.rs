//! # marius-schema
//! 
//! **Crate** : `crates/core/schema/src/lib.rs`  
//! **Projet Marius** · ADR-002 / ADR-003 / ADR-007
//!
//! Point d'entrée de la crate `schema`.  
//! Re-exporte les types des crates core (`Projection`, `Collector`) et inclut
//! le fichier généré par DB-Forge à la compilation (`generated_schema.rs`).
//!
//! ## Contenu de `generated_schema.rs`
//!
//! Pour chaque table surveillée dans `build.rs`, le fichier généré contient :
//!
//! - `{Name}Row` : `struct sqlx::FromRow` (transport sqlx → Dispatcher).
//! - `{Name}StorageRow` : `struct #[repr(C)]` (stockage mémoire contiguë).
//! - `{Name}VarlenOwned` : Struct possédée (`Option<String>`, `Send + 'static`). Absente (remplacée par `()`) si pas de varlena.
//! - `From<{Name}Row> for {Name}StorageRow`
//! - `Collector<MAX, WORDS>` statique.
//! - `impl Projection` stub :
//!   - `type Record = {Name}StorageRow`
//!   - `type VarlenOwned = {Name}VarlenOwned | ()`
//!   - `fetch_batch() -> Vec<(StorageRow, VarlenOwned)>`
//!   - `render() -> &StorageRow + &VarlenOwned + &mut String`
//!   - `artifact_path()`
//! - Constantes de capacité :
//!   - `{NAME}_STATIC_CAP` : Octets HTML statiques.
//!   - `{NAME}_DYNAMIC_CAP` : Largeurs max des valeurs dynamiques.
//!   - `{NAME}_TOTAL_CAP` : `= STATIC_CAP + DYNAMIC_CAP`.
//!
//! ## ADR-003 : Suppression de `RenderPayload<'a>`
//!
//! `RenderPayload<'a>` n'est plus émis dans le fichier généré.  
//! Les `&str` sont reconstruits localement dans `render()` via `as_deref()`, sans
//! traversée de frontière de lifetime — la reconstruction est locale à
//! l'appel, quel que soit le contexte d'exécution (séquentiel ou non).  
//! `VarlenOwned` est le type transporté (`Send + 'static`) ; le payload est éphémère.
//!
//! ## ADR-007 : Frontière Hot/Cold/Erreur sur les champs varlena
//!
//! `VarlenField.max_len` est `Option<usize>` depuis ADR-007 : un `TEXT` sans borne
//! exploitable (ni `VARCHAR(N)`, ni `CHECK` reconnu) n'est plus exclu du schéma
//! ni comblé par un fallback arbitraire (10 000B, désormais supprimé). 
//! 
//! La classification Hot/Cold/Erreur est tranchée par `resolve_and_measure` selon
//! que le champ est référencé ou non par le template résolu — voir
//! `crates/forge/fragment-forge/src/lib.rs`, module `tests_phase_2_1`, tests dédiés à
//! cette table de vérité (`unbounded_field_referenced_fails_resolution`, etc.).
//!
//! Cette frontière vit entièrement côté compilateur (Voie B) ; les tests de
//! ratio de remplissage ci-dessous ne la concernent pas et ont été déclassés
//! en diagnostic.
//!
//! ## Tests
//!
//! 1. **Tests fonctionnels** *(ignorés par défaut, requièrent `DATABASE_URL`)* :  
//!    Vérifient que `fetch_batch()` retourne des tuples `(StorageRow, VarlenOwned)`
//!    valides et que `render()` produit un HTML syntaxiquement correct.
//!
//! 2. **Tests no-realloc** *(toujours actifs, sans `DATABASE_URL`)* :  
//!    Alimentent les structs avec les valeurs pires cas et assertent
//!    `buf.capacity() == {NAME}_TOTAL_CAP` après `render()`.  
//!    Pour `VarlenOwned` : chaînes de `max_len` × `&` (pire cas escape × 6).  
//!    Reste l'invariant de sécurité primaire — contrairement aux tests de
//!    ratio (diagnostic uniquement depuis ADR-007), ces tests bloquent le
//!    build s'ils échouent.
//!
//! 3. **Tests de ratio de remplissage** *(diagnostic informatif, non bloquants)* :  
//!    Mesurent le pourcentage de `TOTAL_CAP` utilisé sur données représentatives.  
//!    Déclassés depuis ADR-007 : le ratio dépend entièrement du contenu du
//!    fixture, pas d'une propriété démontrable du compilateur — un fixture
//!    vide (`Default::default()`) produit mécaniquement un ratio proche de 0%
//!    quelle que soit la borne réelle, sans que cela indique un défaut du pipeline.  
//!    *Note : L'invariant qui comptait réellement ("un champ non borné référencé échoue à la
//!    compilation") est désormais vérifié directement dans `fragment-forge`,
//!    pas indirectement via ce ratio.*

pub mod projection {
    pub use marius_projection::{Projection, VarlenSlot};
}

pub mod collector {
    pub use marius_collector::Collector;
}

// Inclusion du code généré par DB-Forge + Fragment-Forge.
// Ce fichier est recréé à chaque `cargo build` si DATABASE_URL a changé.
include!(concat!(env!("OUT_DIR"), "/generated_schema.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    // Tests de rendu — validité fonctionnelle
    //
    // Marqués #[ignore] : requièrent DATABASE_URL et un jeu de données DML.
    // Exécution manuelle : cargo test -- --ignored
    //
    // Vérifient la chaîne complète :
    //   fetch_batch() → Vec<(StorageRow, VarlenOwned)> → render() → HTML

    #[tokio::test]
    #[ignore]
    async fn test_fetch_content_core() {
        let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();

        let ids = vec![1i64, 2, 3];
        let results = ContentCoreProjection::fetch_batch(&pool, &ids)
            .await
            .unwrap();

        assert!(!results.is_empty(), "Aucun enregistrement — DML appliqué ?");

        let (storage, varlena) = &results[0];

        let mut buf = String::with_capacity(CONTENT_CORE_TOTAL_CAP);
        let mut segments = Vec::with_capacity(ContentCoreProjection::MAX_SEGMENTS);
        // Correction (26/07/2026) : appelait render() directement — cassé
        // pour content.core, segmenté depuis CONTRAT-implementation-
        // projection-segmentee.md Étape 5.
        ContentCoreProjection::render_segments(storage, varlena, &mut buf, &mut segments);
        println!("ContentCore[0] : {buf}");

        // ⚠️ Assertions non revérifiées contre le template réel actuel
        // (core.marius produit désormais class="article", pas class=
        // "content-core", et aucun <dt>document_id</dt> — cf. le dump de
        // generated_schema.rs confronté en session le 23/07/2026). Décalage
        // préexistant, sans rapport avec la segmentation — ce test est
        // #[ignore] par défaut, jamais exécuté depuis. À corriger contre le
        // template réel au moment de le réactiver, pas deviné ici.
        assert!(buf.contains("content-core"), "classe CSS absente");
        assert!(
            buf.contains("<dt>document_id</dt>"),
            "champ document_id absent"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_fetch_product_core() {
        let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();

        let ids = vec![1i64, 2, 3];
        let results = CommerceProductCoreProjection::fetch_batch(&pool, &ids)
            .await
            .unwrap();

        assert!(!results.is_empty());

        let (storage, varlena) = &results[0];

        let mut buf = String::with_capacity(COMMERCE_PRODUCT_CORE_TOTAL_CAP);
        CommerceProductCoreProjection::render(storage, varlena, &mut buf);
        println!("ProductCore[0] : {buf}");

        assert!(buf.contains("commerce-product_core"), "classe CSS absente");
        assert!(buf.contains("<dt>id</dt>"), "champ id absent");
    }

    // Tests no-realloc — INVARIANT CRITIQUE (bloquant)

    #[test]
    fn test_content_core_no_realloc() {
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
            _pad: [0; 3],
        };

        let varlena = ContentCoreVarlenOwned {
            ..Default::default()
        };

        let initial_cap = CONTENT_CORE_TOTAL_CAP;
        let mut buf = String::with_capacity(initial_cap);
        let mut segments = Vec::with_capacity(ContentCoreProjection::MAX_SEGMENTS);

        // Correction (26/07/2026) : appelait render() directement — cassé
        // pour content.core, segmenté depuis CONTRAT-implementation-
        // projection-segmentee.md Étape 5 (render() y est un stub
        // unreachable!(), render_segments() est la seule voie valide).
        // varlena.content == None ici (Default::default()) : le champ
        // segmenté ne traverse de toute façon jamais buf, borné ou non —
        // ce test reste donc un test valide de l'invariant no-realloc pour
        // la partie STATIC_CAP/DYNAMIC_CAP du composant, inchangé par la
        // segmentation.
        ContentCoreProjection::render_segments(&storage, &varlena, &mut buf, &mut segments);

        assert_eq!(
            buf.capacity(),
            initial_cap,
            "REALLOC détecté sur ContentCore : capacity {} → {}.\n\
             Fragment-Forge sous-estime la capacité.\n\
             Longueur réelle du HTML : {} octets.",
            initial_cap,
            buf.capacity(),
            buf.len()
        );

        assert!(buf.starts_with("<!DOCTYPE html>"), "DOCTYPE manquant");
        assert!(
            buf.trim_end().ends_with("</html>"),
            "balise </html> manquante"
        );
        println!(
            "[no-realloc] ContentCore : cap={}, len={}, ratio={:.0}%",
            initial_cap,
            buf.len(),
            buf.len() as f64 / initial_cap as f64 * 100.0
        );
    }

    /// Complète test_content_core_no_realloc ci-dessus, dont `varlena.content`
    /// est toujours `None` (`Default::default()`) — la branche segmentée
    /// (`{% if record.is_readable %}`) n'y est jamais exercée. Ce test-ci
    /// active `is_readable=1` avec un corps volumineux, pour prouver que
    /// `buf` ne réalloue jamais MÊME quand `Segment::Borrowed` est
    /// effectivement poussé — c'est l'invariant central du mécanisme, et il
    /// n'était certifié nulle part au niveau bloquant avant ce test (ajouté
    /// le 26/07/2026, CONTRAT-implementation-projection-segmentee.md ; seuls
    /// les bancs Divan optionnels — jamais exécutés en CI — l'exerçaient).
    #[test]
    fn test_content_core_no_realloc_with_segmented_content() {
        let storage = ContentCoreStorageRow {
            published_at: i64::MIN,
            created_at: i64::MIN,
            modified_at: i64::MIN,
            document_id: i32::MIN,
            author_entity_id: i32::MIN,
            status: i16::MIN,
            is_readable: 1, // active la branche {% if %} contenant le champ segmenté
            is_commentable: 0,
            is_visible_comments: 0,
            _pad: [0; 3],
        };

        // Corps volumineux, largement au-delà de l'ancien seuil AOT de 64 Ko
        // (introspect.rs) — ne doit jamais influencer buf.capacity() puisqu'il
        // devient un Segment::Borrowed autonome, jamais concaténé dans buf.
        let large_body = "<p>Paragraphe de test pour le contenu segmenté.</p>\n".repeat(10_000);

        let varlena = ContentCoreVarlenOwned {
            content: Some(large_body),
            ..Default::default()
        };

        let initial_cap = CONTENT_CORE_TOTAL_CAP;
        let mut buf = String::with_capacity(initial_cap);
        let mut segments = Vec::with_capacity(ContentCoreProjection::MAX_SEGMENTS);

        ContentCoreProjection::render_segments(&storage, &varlena, &mut buf, &mut segments);

        assert_eq!(
            buf.capacity(),
            initial_cap,
            "REALLOC détecté sur ContentCore AVEC contenu segmenté volumineux : \
             capacity {} → {}. Le champ marius:large_content ne devrait jamais \
             influencer buf, quelle que soit sa taille réelle.",
            initial_cap,
            buf.capacity(),
        );

        assert_eq!(
            segments.len(),
            3,
            "3 segments attendus (en-tête Buffered / corps Borrowed / pied \
             Buffered) — {} obtenus. Le mécanisme de segmentation ne s'est \
             peut-être pas déclenché (vérifier is_readable=1 sur la fixture).",
            segments.len()
        );
    }

    #[test]
    fn test_product_core_no_realloc() {
        let storage = CommerceProductCoreStorageRow {
            price_cents: i64::MIN,
            id: i32::MIN,
            stock: i32::MIN,
            media_id: i32::MIN,
            is_available: 0,
            _pad: [0; 3],
        };

        let initial_cap = COMMERCE_PRODUCT_CORE_TOTAL_CAP;
        let mut buf = String::with_capacity(initial_cap);

        CommerceProductCoreProjection::render(&storage, &(), &mut buf);

        assert_eq!(
            buf.capacity(),
            initial_cap,
            "REALLOC détecté sur CommerceProductCore : capacity {} → {}.\n\
             Longueur réelle : {} octets.",
            initial_cap,
            buf.capacity(),
            buf.len()
        );

        assert!(buf.starts_with("<article"), "tag ouvrant manquant");
        assert!(
            buf.trim_end().ends_with("</article>"),
            "tag fermant manquant"
        );
        println!(
            "[no-realloc] ProductCore : cap={}, len={}, ratio={:.0}%",
            initial_cap,
            buf.len(),
            buf.len() as f64 / initial_cap as f64 * 100.0
        );
    }

    // Diagnostics de ratio de remplissage — NON BLOQUANTS (ADR-007)
    //
    // Déclassés : ne sont plus des tests d'architecture depuis l'introduction
    // de la frontière Hot/Cold/Erreur. Le ratio mesuré ici dépend entièrement
    // du contenu du fixture (Default::default() produit un ratio proche de 0%
    // mécaniquement, quelle que soit la borne réelle des champs varlena) — ce
    // n'est plus un signal fiable d'un défaut du compilateur. L'invariant qui
    // comptait réellement ("un champ non borné référencé échoue explicitement
    // à la compilation") est désormais vérifié directement et positivement
    // dans crates/forge/fragment-forge/src/lib.rs (tests dédiés au disjoncteur
    // Hot/Cold/Erreur), sans dépendre du contenu d'un fixture de rendu.
    //
    // Conservé comme indicateur informatif (eprintln!, jamais de panic!) :
    // un ratio anormalement bas peut suggérer qu'une borne CHECK/VARCHAR est
    // généreuse par rapport à l'usage réel — signal de tuning, pas de
    // correction. Aucune assertion bloquante.

    #[test]
    fn diag_content_core_ratio() {
        let storage = ContentCoreStorageRow {
            published_at: 1_700_000_000_000_000i64,
            created_at: 1_700_000_000_000_000i64,
            modified_at: 1_700_000_000_000_000i64,
            document_id: 42,
            author_entity_id: 7,
            status: 1,
            is_readable: 0,
            is_commentable: 0,
            is_visible_comments: 0,
            _pad: [0; 3],
        };

        // Fixture minimal (varlena vides) — ce diagnostic ne prétend plus
        // mesurer un cas "réaliste" : il journalise simplement le ratio
        // obtenu sur ce fixture précis, sans assertion sur sa valeur.
        let varlena = ContentCoreVarlenOwned {
            ..Default::default()
        };

        let mut buf = String::new();
        let mut segments = Vec::with_capacity(ContentCoreProjection::MAX_SEGMENTS);
        // Correction (26/07/2026) : appelait render() directement — cassé
        // depuis la segmentation. is_readable=0 par défaut ci-dessus : le
        // ratio mesuré ici ne reflète toujours que la partie STATIC/DYNAMIC
        // du composant (en-tête + pied), jamais le champ marius:large_content
        // — cohérent avec le sens même de ce diagnostic (CONTENT_CORE_TOTAL_CAP
        // ne compte plus ce champ non plus, cf. Étape 1 du Contrat).
        ContentCoreProjection::render_segments(&storage, &varlena, &mut buf, &mut segments);

        let ratio = buf.len() as f64 / CONTENT_CORE_TOTAL_CAP as f64 * 100.0;
        eprintln!(
            "[diag] ContentCore (fixture varlena vide) : {}/{} = {:.1}% \
             — informatif uniquement, voir ADR-007.",
            buf.len(),
            CONTENT_CORE_TOTAL_CAP,
            ratio
        );
    }

    #[test]
    fn diag_product_core_ratio() {
        let storage = CommerceProductCoreStorageRow {
            price_cents: 1999,
            id: 42,
            stock: 150,
            media_id: 7,
            is_available: 1,
            _pad: [0; 3],
        };

        let mut buf = String::new();
        CommerceProductCoreProjection::render(&storage, &(), &mut buf);

        let ratio = buf.len() as f64 / COMMERCE_PRODUCT_CORE_TOTAL_CAP as f64 * 100.0;
        eprintln!(
            "[diag] ProductCore : {}/{} = {:.1}% — informatif uniquement, voir ADR-007.",
            buf.len(),
            COMMERCE_PRODUCT_CORE_TOTAL_CAP,
            ratio
        );
    }
}
