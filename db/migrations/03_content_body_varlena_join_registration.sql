-- ==============================================================================
-- db/migrations/03_content_body_varlena_join_registration.sql
-- Enregistrement de la jointure varlena content.core → content.body dans
-- meta.component_varlena_join (registre d'intention, 01_meta/01_tables.sql).
--
-- RÉVISION (arbitrage du 22/07/2026) : le handoff initial dictait
-- component_id = 'content.document'. Confronté au seed réel
-- (10_meta_seed/01_manifest.sql), le seul précédent existant rattache déjà un
-- join varlena à 'content.core' (slot 0, vers content.identity) — pas à
-- 'content.document', qui est la spine identifiant pure (id, doc_type), sans
-- VarlenOwned associé. Arbitré : le code et l'état réel font autorité sur le
-- handoff. component_id = 'content.core'.
--
-- Valeurs retenues :
--   component_id  = 'content.core'
--   ref_schema    = 'content'
--   ref_table     = 'body'
--   fk_column     = 'document_id'   -- FK relationnelle standard (content.body.document_id
--                                      → content.document.id), cas 1:1 couvert par le
--                                      correctif JOIN de Phase 1 (Étape 5 du Contrat).
--   join_slot_idx = 1               -- slot 0 déjà occupé par ('content.core', 0, 'content',
--                                      'identity', 'document_id') — posé par le seed initial.
--
-- Prérequis vérifié (seed 01_manifest.sql confirmé) : 'content.core' existe bien
-- dans meta.containment_intent. Garde-fou conservé malgré tout — la migration ne
-- doit pas supposer un état qu'elle n'a pas elle-même vérifié à l'exécution.
--
-- Exécution : psql "postgresql://.../marius" -f db/migrations/03_content_body_varlena_join_registration.sql
-- ==============================================================================

BEGIN;

-- ==============================================================================
-- 1. Garde-fou : le composant parent doit être déclaré dans le registre d'intention
-- ==============================================================================

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM meta.containment_intent
        WHERE component_id = 'content.core'
    ) THEN
        RAISE EXCEPTION
            'meta.containment_intent ne contient aucune ligne component_id=''content.core''. Le registre d''intention doit déclarer le composant avant qu''une jointure varlena ne lui soit attachée. Insertion interrompue.';
    END IF;
END;
$$;

-- ==============================================================================
-- 2. Insertion idempotente de la jointure
-- ==============================================================================
-- Idempotent par choix (IF NOT EXISTS + RAISE NOTICE) plutôt que ON CONFLICT
-- DO NOTHING silencieux : une ré-exécution de cette migration doit informer,
-- pas masquer, si la ligne est déjà présente.

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM meta.component_varlena_join
        WHERE component_id = 'content.core'
          AND join_slot_idx = 1
    ) THEN
        RAISE NOTICE 'meta.component_varlena_join : ligne (content.core, slot 1) déjà présente — aucune insertion.';
    ELSE
        INSERT INTO meta.component_varlena_join
            (component_id, join_slot_idx, ref_schema, ref_table, fk_column)
        VALUES
            ('content.core', 1, 'content', 'body', 'document_id');
    END IF;
END;
$$;

COMMIT;

-- ==============================================================================
-- Vérification
-- ==============================================================================
SELECT component_id, join_slot_idx, ref_schema, ref_table, fk_column
FROM meta.component_varlena_join
WHERE component_id = 'content.core'
ORDER BY join_slot_idx;
