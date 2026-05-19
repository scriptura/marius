-- ==============================================================================
-- DML de test — Prototype DB-Forge
-- Projet Marius · PostgreSQL 18
-- Cible  : content.core + commerce.product_core
-- Objectif : MAX(pk) > 0 dans les deux tables surveillées pour que
--            fetch_max_id() calcule un MAX_ENTITY_ID non-trivial,
--            et que sqlx::query_as! valide les types au build-time.
--
-- Exécution : psql -U marius_admin -d marius -h localhost -W \
--               -f db/tests/dml_forge_test.pgsql
-- Prérequis : tables vides. Les IDs GENERATED ALWAYS AS IDENTITY commencent à 1.
-- ==============================================================================

-- marius_admin a BYPASSRLS (ALTER ROLE marius_admin BYPASSRLS dans 08_dcl).
-- SET row_security = off garantit que les politiques SELECT/UPDATE ne bloquent
-- pas les inserts de seed, indépendamment du contexte de session.
SET row_security = off;

-- ==============================================================================
-- 1. Spines et dépendances amont
-- ==============================================================================

-- identity.entity — requis par content.core.author_entity_id (FK nullable, SET NULL)
-- Cinq entités actives (anonymized_at = NULL).
INSERT INTO identity.entity (anonymized_at) VALUES
    (NULL),  -- entity_id = 1
    (NULL),  -- entity_id = 2
    (NULL),  -- entity_id = 3
    (NULL),  -- entity_id = 4
    (NULL);  -- entity_id = 5

-- content.document — spine de content.core
-- Variété de doc_type : 0=article, 1=page, 2=billet, 3=newsletter
INSERT INTO content.document (doc_type) VALUES
    (0),  -- document_id = 1  article
    (1),  -- document_id = 2  page
    (0),  -- document_id = 3  article
    (2),  -- document_id = 4  billet
    (3);  -- document_id = 5  newsletter

-- ==============================================================================
-- 2. content.core — table surveillée n°1
-- ==============================================================================
-- Couverture des cas nullable :
--   published_at  : présent (status=1) ou absent (status=0, status=9)
--   modified_at   : présent ou absent
--   author_entity_id : présent ou NULL (auteur anonymisé RGPD)
--
-- status : 0=brouillon · 1=publié · 2=en révision · 9=archivé
INSERT INTO content.core (
    published_at,
    created_at,
    modified_at,
    document_id,
    author_entity_id,
    status,
    is_readable,
    is_commentable,
    is_visible_comments
) VALUES
    -- 1 : publié, auteur connu, tous les champs non-null remplis
    (NOW() - INTERVAL '2 days',
     NOW() - INTERVAL '5 days',
     NOW() - INTERVAL '1 day',
     1, 1, 1, true, true, true),

    -- 2 : brouillon, published_at absent, modified_at récent
    (NULL,
     NOW() - INTERVAL '3 days',
     NOW() - INTERVAL '2 hours',
     2, 1, 0, true, false, false),

    -- 3 : publié, auteur anonymisé (NULL → sentinel 0 dans le Store repr(C))
    (NOW() - INTERVAL '30 days',
     NOW() - INTERVAL '30 days',
     NULL,
     3, NULL, 1, true, true, true),

    -- 4 : publié, commentaires désactivés, modified_at présent
    (NOW() - INTERVAL '10 days',
     NOW() - INTERVAL '10 days',
     NOW() - INTERVAL '5 days',
     4, 2, 1, true, false, false),

    -- 5 : archivé, tous les champs nullable présents
    (NOW() - INTERVAL '90 days',
     NOW() - INTERVAL '90 days',
     NOW() - INTERVAL '1 day',
     5, 3, 9, false, false, false);

-- ==============================================================================
-- 3. commerce.product_core — table surveillée n°2
-- ==============================================================================
-- Couverture des cas nullable :
--   price_cents = NULL  → sentinel -1 dans le Store repr(C)
--   price_cents = 0     → produit gratuit (CHECK >= 0, sentinel -1 ≠ 0 : correct)
--   media_id    = NULL  → sentinel 0 (FK cross-schéma non encore liée)
--
-- fillfactor=80 : les UPDATE stock sont des HOT updates (non indexé).
INSERT INTO commerce.product_core (price_cents, stock, media_id, is_available) VALUES
    -- 1 : produit standard disponible
    (1999,  50,  NULL, true),

    -- 2 : produit gratuit — price_cents=0 valide (CHECK >= 0)
    --     Valide le sentinel -1 ≠ 0 dans le Store repr(C)
    (0,    100,  NULL, true),

    -- 3 : produit premium, stock limité
    (9900,  10,  NULL, true),

    -- 4 : produit épuisé, disponible dès réapprovisionnement
    (2499,   0,  NULL, false),

    -- 5 : tarif non défini (NULL) — produit en cours de création
    --     Valide le sentinel -1 dans ContentCore.price_cents
    (NULL,   0,  NULL, false);

-- ==============================================================================
-- Vérification post-insertion
-- ==============================================================================
-- Attendu : 5 lignes, max_pk = 5 pour chaque table.
-- Ces valeurs permettent au build.rs de calculer :
--   content.core       : MAX(document_id)=5 → +20%=6 → 1 word → power-of-two=1 → MAX=64
--   commerce.product_core : MAX(id)=5 → idem → MAX=64
-- Après ANALYZE, la ECSM recalcule les densités réelles.
SELECT
    'content.core'            AS table_name,
    COUNT(*)                  AS nb_lignes,
    MAX(document_id)          AS max_pk
FROM content.core
UNION ALL
SELECT
    'commerce.product_core',
    COUNT(*),
    MAX(id)
FROM commerce.product_core;

-- ANALYZE pour que pg_stats reflète les données réelles (densité varlena, ECSM).
ANALYZE content.core;
ANALYZE commerce.product_core;

-- Restaurer le comportement RLS par défaut de la session.
RESET row_security;
