-- ==============================================================================
-- 10_meta_seed/01_manifest.sql
-- Architecture ECS/DOD · Projet Marius · PostgreSQL 18
-- Contenu : manifeste des invariants AOT — TRUNCATE + INSERT meta.containment_intent
--
-- Source   : meta_data.sql v2.1 + meta_registry.sql v2.2 (fusion)
-- Format   : v2.2 — naive_density_bytes SMALLINT (optionnel)
--            exempt_bloat_check BOOLEAN
--            mutation_procedures TEXT[] (signatures to_regprocedure())
--            immutable_keys name[]
--
-- Pré-requis : toutes les tables physiques existent (étapes 02–06 chargées).
--   to_regclass() résout correctement → component_not_found_alert = FALSE.
--   La requête de vérification finale retourne des résultats fiables.
--
-- Correctifs v2.1 documentés :
--   identity.person_identity : intent 72 → 80B (varlena avg_width post-ANALYZE)
--   identity.person_biography : intent 44 → 48B (tail MAXALIGN omis v1)
--   identity.role : exempt_bloat_check = true (7 lignes, fraction de page)
-- ==============================================================================

BEGIN;

-- Nettoyage du registre pour éviter les doublons lors des ré-exécutions
TRUNCATE meta.component_varlena_join, meta.containment_intent;

-- ==============================================================================
-- INSERTION DES INVARIANTS PAR DOMAINE
-- Format mutation_procedures : TEXT[] — signatures canoniques PostgreSQL
--   (character varying, not varchar ; integer, not int — requis par to_regprocedure)
-- ==============================================================================

INSERT INTO meta.containment_intent
    (component_id, intent_density_bytes, rls_guard_bitmask,
     mutation_procedures, immutable_keys, exempt_bloat_check, naive_density_bytes)
VALUES

-- ── DOMAINE IDENTITY ──────────────────────────────────────────────────────────

-- identity.auth
(
    'identity.auth',
    56,
    1,
    ARRAY[
        'identity.create_account(character varying,character varying,character varying,smallint,character varying)',
        'identity.record_login(integer)',
        'identity.anonymize_person(integer)'
    ],
    ARRAY['entity_id'::name, 'created_at'::name],
    false,
    NULL
),

-- identity.person_identity
(
    'identity.person_identity',
    40,
    256,
    ARRAY[
        'identity.create_person(character varying,character varying,smallint,smallint)'
    ],
    ARRAY['entity_id'::name],
    false,
    NULL
),

-- identity.person_biography
(
    'identity.person_biography',
    44,
    256,
    NULL,
    ARRAY['entity_id'::name],
    false,
    NULL
),

-- identity.role — exempt_bloat_check = true
(
    'identity.role',
    32,
    NULL,
    NULL,
    NULL,
    true,
    NULL
),


-- ── DOMAINE CONTENT ───────────────────────────────────────────────────────────

-- content.document — spine : id INT4 + doc_type INT2 + 2B pad
-- Header 24B (2 cols, pas de bitmap) + 4B + 2B = 30B → MAXALIGN = 32B
(
    'content.document',
    32,
    NULL,
    ARRAY[
        'content.create_document(integer,character varying,character varying,smallint,smallint,text,character varying,character varying)'
    ],
    ARRAY['entity_id'::name],
    false,
    NULL
),

-- content.core — Layout : 3×TSTZ(24B) + doc_id INT4(4) + author INT4(4)
--                         + status INT2(2) + 3×BOOL(3B) + 1B pad
-- Header 32B (9 cols, null bitmap 2B, MAXALIGN 32B) + 38B données = 70B → MAXALIGN = 72B
(
    'content.core',
    72,
    32768,
    ARRAY[
        'content.create_document(integer,character varying,character varying,smallint,smallint,text,character varying,character varying)',
        'content.publish_document(integer)'
    ],
    ARRAY['document_id'::name, 'created_at'::name],
    false,
    NULL
),

-- content.content_to_tag — content_id INT4 + tag_id INT4
-- Header 24B (2 cols) + 8B = 32B → MAXALIGN = 32B
(
    'content.content_to_tag',
    32,
    NULL,
    NULL,
    ARRAY['document_id'::name, 'tag_id'::name],
    false,
    NULL
),

-- content.tag_hierarchy
(
    'content.tag_hierarchy',
    36,
    2048,
    ARRAY[
        'content.create_tag(character varying,character varying,integer)'
    ],
    ARRAY['ancestor_id'::name, 'descendant_id'::name, 'depth'::name],
    false,
    NULL
),


-- ── DOMAINE COMMERCE ──────────────────────────────────────────────────────────

-- commerce.product_core — fillfactor=80
-- Layout : price_cents INT8(8) + id INT4(4) + stock INT4(4) + media_id INT4(4)
--          + is_available BOOL(1) + 3B pad
-- Header 24B (5 cols, bitmap 1B, MAXALIGN 24B) + 24B = 48B → MAXALIGN = 48B
(
    'commerce.product_core',
    48,
    262144,
    ARRAY[
        'commerce.create_product(character varying,character varying,bigint,integer,character varying)',
        'commerce.create_transaction_item(integer,integer,integer)'
    ],
    ARRAY['id'::name],
    false,
    NULL
),

-- commerce.transaction_core
(
    'commerce.transaction_core',
    64,
    NULL,
    ARRAY[
        'commerce.create_transaction(integer,integer,smallint,smallint,text)'
    ],
    ARRAY['client_entity_id'::name, 'created_at'::name],
    false,
    NULL
),

-- commerce.transaction_item — 0 varlena, 0 nullable
-- Layout : unit_price INT8(8) + transaction_id INT4(4) + product_id INT4(4) + quantity INT4(4)
-- Header 24B (4 cols, pas de bitmap) + 20B = 44B → MAXALIGN = 48B
(
    'commerce.transaction_item',
    48,
    NULL,
    ARRAY[
        'commerce.create_transaction_item(integer,integer,integer)'
    ],
    ARRAY['unit_price_snapshot_cents'::name, 'transaction_id'::name, 'product_id'::name],
    false,
    NULL
),

-- commerce.transaction_price — Layout : 3×INT8(24B) + transaction_id INT4(4)
--                                        + tax_rate_bp INT4(4) + currency_code INT2(2)
--                                        + is_tax_included BOOL(1) + 1B pad
-- Header 24B (7 cols, pas de bitmap) + 36B = 60B → MAXALIGN = 64B
(
    'commerce.transaction_price',
    64,
    262144,
    ARRAY[
        'commerce.create_transaction(integer,integer,smallint,smallint,text)'
    ],
    ARRAY['transaction_id'::name],
    false,
    NULL
),


-- ── DOMAINE ORG ───────────────────────────────────────────────────────────────

-- org.org_hierarchy — nested set
-- Layout : entity_id INT4(4) + lft INT4(4) + rgt INT4(4) + depth INT2(2) + 2B pad
-- Header 24B (4 cols, pas de bitmap) + 14B = 38B → MAXALIGN = 40B
(
    'org.org_hierarchy',
    40,
    128,
    ARRAY[
        'org.create_organization(character varying,character varying,character varying,integer,integer)',
        'org.add_organization_to_hierarchy(integer,integer)'
    ],
    ARRAY['entity_id'::name],
    false,
    NULL
),


-- ── DOMAINE GEO ───────────────────────────────────────────────────────────────

-- geo.place_core
(
    'geo.place_core',
    32,
    NULL,
    ARRAY[
        'geo.create_place(character varying,smallint,smallint,double precision,double precision,smallint,character varying,character varying,character varying,character varying)'
    ],
    ARRAY['entity_id'::name],
    false,
    NULL
);

-- ── JOINTURES VARLENA ─────────────────────────────────────────────────────────
-- Phase 1 : join_slot_idx = 0 (slot unique par composant).
-- Phase 2 : slots supplémentaires ajoutés ici pour les multi-JOIN.

INSERT INTO meta.component_varlena_join
    (component_id, join_slot_idx, ref_schema, ref_table, fk_column)
VALUES
    ('content.core', 0, 'content', 'identity', 'document_id'),
    ('content.core', 1, 'content', 'body', 'document_id');

-- ── COMMIT ────────────────────────────────────────────────────────────────────

COMMIT;

-- ==============================================================================
-- VÉRIFICATION IMMÉDIATE DU DRIFT
-- Affiche les composants qui ne respectent pas le contrat dès l'initialisation.
-- Pré-requis : ANALYZE exécuté sur les tables pour des métriques varlena fiables.
-- ==============================================================================
SELECT
    component_name,
    intent_density_bytes,
    actual_density_bytes,
    (actual_density_bytes - intent_density_bytes) AS padding_overhead,
    density_drift_alert,
    component_not_found_alert,
    exempt_bloat_check
FROM meta.v_extended_containment_security_matrix
ORDER BY density_drift_alert DESC, component_not_found_alert DESC, component_name;
