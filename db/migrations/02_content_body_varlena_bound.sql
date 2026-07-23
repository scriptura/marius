-- ==============================================================================
-- db/migrations/02_content_body_varlena_bound.sql
-- Borne content.body.content : TEXT non borné → VARCHAR(200000).
-- Prérequis pour l'exercice du mécanisme varlena (ADR-007 §3.1, §6 — alternative
-- retenue) sur ce champ. Sans cette borne, introspect.rs ne peut pas résoudre
-- max_len statiquement pour ce composant (ADR-007 §4 : Hot sans borne connue
-- = ResolverError::UnboundedField, une fois le champ référencé par un template).
--
-- Exécution : psql "postgresql://.../marius" -f db/migrations/02_content_body_varlena_bound.sql
--
-- Stratégie : ALTER COLUMN TYPE déclenche un rewrite complet de la table
-- (relation TOAST incluse). PostgreSQL applique la contrainte de longueur au
-- moment du rewrite, mais un rejet à ce stade survient après avoir engagé une
-- opération coûteuse, sans diagnostic exploitable. Scan de validation explicite
-- en amont (bloc 1) — fail-fast avant tout rewrite, conforme à article-0.md §4.
--
-- Révision (exécution réelle du 22/07/2026) : content.v_article dépend de
-- content.body.content (règle _RETURN) — non présente dans les fichiers DDL
-- versionnés fournis à ce jour, dépendance découverte uniquement par confron-
-- tation au schéma réel. ALTER COLUMN TYPE refuse toute colonne référencée par
-- une vue. Contournement : DROP VIEW / recréation à l'identique autour de
-- l'ALTER, GRANTs réappliqués explicitement (un DROP VIEW ne les préserve pas).
-- Vérifié sans dépendant en cascade (pg_depend, deptype='n' : 0 ligne) et sans
-- security_barrier/reloptions à répliquer (reloptions : NULL).
-- ==============================================================================

BEGIN;

-- ==============================================================================
-- 1. Scan de validation pré-migration
-- ==============================================================================
-- Toute ligne existante dépassant 200000 caractères interrompt la migration
-- avant le rewrite, avec le décompte et la longueur max observée pour
-- diagnostic — pas de troncature silencieuse, pas d'échec opaque.

DO $$
DECLARE
    v_count INT;
    v_max   INT;
BEGIN
    SELECT COUNT(*), COALESCE(MAX(length(content)), 0)
    INTO   v_count, v_max
    FROM   content.body
    WHERE  length(content) > 200000;

    IF v_count > 0 THEN
        RAISE EXCEPTION
            'content.body : % ligne(s) dépassent la borne 200000 caractères (longueur max observée : %). Migration interrompue — aucune donnée modifiée.',
            v_count, v_max
            USING ERRCODE = 'check_violation';
    END IF;
END;
$$;

-- ==============================================================================
-- 2. Suppression temporaire de la vue dépendante
-- ==============================================================================
-- content.v_article référence b.content directement (LEFT JOIN content.body b).
-- Définition capturée par confrontation au schéma réel (pg_get_viewdef), pas
-- reconstruite de mémoire. GRANTs (relacl) capturés avant DROP, réappliqués
-- au bloc 4 : postgres=owner, marius_admin=INSERT/UPDATE/DELETE, marius_user=SELECT.

DROP VIEW content.v_article;

-- ==============================================================================
-- 3. Changement de type
-- ==============================================================================
-- VARCHAR(200000) : même représentation physique varlena que TEXT (ADR-007 H5,
-- seule hypothèse structurellement garantie par PostgreSQL — atttypmod encode
-- directement la borne, zéro parsing de contrainte). Le rewrite déclenché ici
-- est le scan de validation matériel — le bloc 1 ne fait qu'anticiper l'échec
-- avec un message exploitable.

ALTER TABLE content.body
    ALTER COLUMN content TYPE VARCHAR(200000);

-- Réaffirmation explicite de STORAGE EXTENDED : posé sur la colonne TEXT
-- d'origine (05_content/01_components.sql), non supposé survivre implicitement
-- au changement de type — réaffirmé explicitement, idempotent, coût nul.
ALTER TABLE content.body
    ALTER COLUMN content SET STORAGE EXTENDED;

-- ==============================================================================
-- 4. Recréation de la vue à l'identique + réapplication des GRANTs
-- ==============================================================================

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

SELECT relacl, reloptions
FROM pg_class
WHERE oid = 'content.v_article'::regclass;
