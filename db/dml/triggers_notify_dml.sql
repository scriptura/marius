-- ==============================================================================
-- db/dml/triggers_notify.sql
-- Triggers LISTEN/NOTIFY pour le pipeline réactif Marius
-- Projet Marius · PostgreSQL 18
--
-- Exécution : psql -U postgres -d marius -h localhost -f db/dml/triggers_notify.sql
--
-- Ces triggers alimentent le Collector<MAX, WORDS> via pg_notify.
-- Le payload est l'ID entier de la ligne modifiée (texte).
-- Le canal correspond à la constante écoutée dans server/main.rs.
-- ==============================================================================

-- ==============================================================================
-- content.core → canal 'content_core_updates'
-- PK : document_id
-- ==============================================================================
CREATE OR REPLACE FUNCTION content.notify_core_change()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = content, pg_temp
AS $$
BEGIN
    -- INSERT et UPDATE : notifier avec le document_id
    -- DELETE : notifier également (le Dispatcher gère les IDs absents)
    PERFORM pg_notify(
        'content_core_updates',
        COALESCE(NEW.document_id, OLD.document_id)::text
    );
    RETURN COALESCE(NEW, OLD);
END;
$$;

DROP TRIGGER IF EXISTS trg_content_core_notify ON content.core;
CREATE TRIGGER trg_content_core_notify
AFTER INSERT OR UPDATE OR DELETE ON content.core
FOR EACH ROW EXECUTE FUNCTION content.notify_core_change();

-- ==============================================================================
-- commerce.product_core → canal 'product_core_updates'
-- PK : id
-- ==============================================================================
CREATE OR REPLACE FUNCTION commerce.notify_product_change()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = commerce, pg_temp
AS $$
BEGIN
    PERFORM pg_notify(
        'product_core_updates',
        COALESCE(NEW.id, OLD.id)::text
    );
    RETURN COALESCE(NEW, OLD);
END;
$$;

DROP TRIGGER IF EXISTS trg_product_core_notify ON commerce.product_core;
CREATE TRIGGER trg_product_core_notify
AFTER INSERT OR UPDATE OR DELETE ON commerce.product_core
FOR EACH ROW EXECUTE FUNCTION commerce.notify_product_change();

-- ==============================================================================
-- Vérification
-- ==============================================================================
SELECT
    trigger_schema,
    trigger_name,
    event_object_table,
    event_manipulation,
    action_timing
FROM information_schema.triggers
WHERE trigger_name IN ('trg_content_core_notify', 'trg_product_core_notify')
ORDER BY trigger_name, event_manipulation;
