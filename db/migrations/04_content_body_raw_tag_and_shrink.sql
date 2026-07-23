-- ==============================================================================
-- db/migrations/04_content_body_raw_tag_and_shrink.sql
-- content.body.content : tag 'marius:raw' + réduction VARCHAR(200000) → VARCHAR(32000).
--
-- CONTRAT-implementation-varlena-raw.md, Étape 7. Prérequis : Étapes 1-6 closes
-- côté code (EscapePolicy, détection du tag, calcul de capacité, codegen
-- buf.push_str pour Raw) — cette migration n'a d'effet utile qu'une fois ce
-- code réellement compilé et testé. Ne pas exécuter avant confirmation.
--
-- Pourquoi 32000, pas 200000 : avec EscapePolicy::Raw, le facteur de capacité
-- est 1 (pas 6) — max_escaped_len = max_len directement. 200000 > 65536 (seuil
-- AOT absolu, introspect.rs) resterait bloquant même à facteur 1. 32000 est un
-- choix PoC délibéré (< 65536, marge confortable) — pas une contrainte produit
-- réelle sur la taille d'un article, cf. TODO chunking déjà posé dans
-- introspect.rs pour la suite.
--
-- Exécution : psql "postgresql://.../marius" -f db/migrations/04_content_body_raw_tag_and_shrink.sql
-- ==============================================================================

BEGIN;

-- ==============================================================================
-- 1. Scan de validation pré-migration
-- ==============================================================================
-- Toute ligne existante dépassant 32000 caractères interrompt la migration
-- avant le rewrite — même discipline que la migration 02.

DO $$
DECLARE
    v_count INT;
    v_max   INT;
BEGIN
    SELECT COUNT(*), COALESCE(MAX(length(content)), 0)
    INTO   v_count, v_max
    FROM   content.body
    WHERE  length(content) > 32000;

    IF v_count > 0 THEN
        RAISE EXCEPTION
            'content.body : % ligne(s) dépassent la borne 32000 caractères (longueur max observée : %). Migration interrompue — aucune donnée modifiée.',
            v_count, v_max
            USING ERRCODE = 'check_violation';
    END IF;
END;
$$;

-- ==============================================================================
-- 2. Suppression temporaire de la vue dépendante (inchangé depuis migration 02)
-- ==============================================================================

DROP VIEW content.v_article;

-- ==============================================================================
-- 3. Réduction de type + tag marius:raw
-- ==============================================================================

ALTER TABLE content.body
    ALTER COLUMN content TYPE VARCHAR(32000);

-- STORAGE EXTENDED toujours pertinent à 32000B (bien au-dessus du seuil TOAST
-- ~2000B) — réaffirmé explicitement, comme en migration 02.
ALTER TABLE content.body
    ALTER COLUMN content SET STORAGE EXTENDED;

-- Tag marius:raw (CONTRAT-implementation-varlena-raw.md) : certifie que ce
-- contenu est du HTML déjà constitué, à injecter tel quel — introspect.rs le
-- détecte via pg_description, fragment-forge en tire EscapePolicy::Raw
-- (buf.push_str direct, jamais marius_html_escape). Distinct de
-- 'marius:pre_escaped', qui échappe quand même au runtime — ne jamais
-- utiliser ce tag pour un contenu réellement destiné à être échappé.
COMMENT ON COLUMN content.body.content IS 'marius:raw';

-- ==============================================================================
-- 4. Recréation de la vue à l'identique + réapplication des GRANTs
-- ==============================================================================
-- Définition et GRANTs identiques à la migration 02 — seul le type de la
-- colonne source (content.body.content) a changé, la vue elle-même n'y fait
-- aucune référence de type explicite.

CREATE VIEW content.v_article AS
SELECT d.id AS identifier,
       d.doc_type,
       ci.headline,
       ci.slug,
       ci.alternative_headline,
       ci.description,
       co.status,
       co.is_readable,
       co.is_commentable,
       co.published_at,
       co.created_at,
       co.modified_at,
       co.author_entity_id AS author_id,
       b.content AS article_body,
       ( SELECT json_agg(json_build_object('id', t.id, 'name', t.name, 'slug', t.slug) ORDER BY t.name)
            FROM content.content_to_tag ct
            JOIN content.tag t ON t.id = ct.tag_id
           WHERE ct.content_id = d.id) AS keywords,
       ( SELECT json_agg(json_build_object('id', m.id, 'name', mc.name,
                          'url', (m.folder_url::text || '/'::text) || m.file_name::text,
                          'mime_type', m.mime_type, 'width', m.width, 'height', m.height,
                          'position', ctm."position") ORDER BY ctm."position")
            FROM content.content_to_media ctm
            JOIN content.media_core m ON m.id = ctm.media_id
            LEFT JOIN content.media_content mc ON mc.media_id = m.id
           WHERE ctm.content_id = d.id) AS images
FROM content.document d
JOIN content.core co ON co.document_id = d.id
JOIN content.identity ci ON ci.document_id = d.id
LEFT JOIN content.body b ON b.document_id = d.id
WHERE co.status = 1
   OR (identity.rls_auth_bits() & 16) = 16
   OR (identity.rls_auth_bits() & 32768) = 32768
   OR co.author_entity_id = identity.rls_user_id();

GRANT INSERT, UPDATE, DELETE ON content.v_article TO marius_admin;
GRANT SELECT ON content.v_article TO marius_user;

COMMIT;

-- ==============================================================================
-- Vérification
-- ==============================================================================
SELECT
    table_schema || '.' || table_name AS table_fqn,
    column_name,
    data_type,
    character_maximum_length
FROM information_schema.columns
WHERE table_schema = 'content'
  AND table_name    = 'body'
  AND column_name   = 'content';

SELECT
    a.attname AS column_name,
    col_description(a.attrelid, a.attnum) AS tag
FROM pg_attribute a
WHERE a.attrelid = 'content.body'::regclass
  AND a.attname   = 'content'
  AND NOT a.attisdropped;

SELECT relacl, reloptions
FROM pg_class
WHERE oid = 'content.v_article'::regclass;
