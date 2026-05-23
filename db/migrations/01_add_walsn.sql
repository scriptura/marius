-- ==============================================================================
-- db/migrations/01_add_walsn.sql
-- Injection de la colonne walsn pg_lsn sur les tables surveillées.
-- Prérequis pour le mécanisme de resync LSN de la Phase 2 (SHM / Logical Decoding).
--
-- Exécution : psql -U postgres -d marius -h localhost -f db/migrations/01_add_walsn.sql
--
-- Stratégie : trigger BEFORE INSERT OR UPDATE plutôt que modification de chaque
-- procédure SECURITY DEFINER — garantit que walsn est toujours peuplé
-- indépendamment du chemin d'écriture, y compris les migrations futures.
-- ==============================================================================

BEGIN;

-- ==============================================================================
-- 1. Ajout des colonnes
-- ==============================================================================

ALTER TABLE content.core
    ADD COLUMN IF NOT EXISTS walsn pg_lsn NOT NULL DEFAULT '0/0'::pg_lsn;

ALTER TABLE commerce.product_core
    ADD COLUMN IF NOT EXISTS walsn pg_lsn NOT NULL DEFAULT '0/0'::pg_lsn;

-- ==============================================================================
-- 2. Fonction trigger commune (peuple walsn via pg_current_wal_lsn)
-- ==============================================================================
-- Une seule fonction suffit — elle est attachée à chaque table.
-- SECURITY DEFINER n'est pas requis ici : le trigger s'exécute dans le
-- contexte de la transaction appelante, qui a déjà passé le RLS et les
-- procédures scellées. pg_current_wal_lsn() est accessible à tous les rôles.

CREATE OR REPLACE FUNCTION meta.stamp_walsn()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    NEW.walsn := pg_current_wal_lsn();
    RETURN NEW;
END;
$$;

-- ==============================================================================
-- 3. Attachement des triggers
-- ==============================================================================

DROP TRIGGER IF EXISTS trg_stamp_walsn ON content.core;
CREATE TRIGGER trg_stamp_walsn
BEFORE INSERT OR UPDATE ON content.core
FOR EACH ROW EXECUTE FUNCTION meta.stamp_walsn();

DROP TRIGGER IF EXISTS trg_stamp_walsn ON commerce.product_core;
CREATE TRIGGER trg_stamp_walsn
BEFORE INSERT OR UPDATE ON commerce.product_core
FOR EACH ROW EXECUTE FUNCTION meta.stamp_walsn();

-- ==============================================================================
-- 4. Backfill : peupler walsn pour les lignes existantes
-- ==============================================================================
-- pg_current_wal_lsn() en backfill n'est qu'une approximation (le LSN réel
-- de chaque ligne est perdu). Acceptable pour Phase 1 — le resync Phase 2
-- utilisera ces valeurs uniquement comme point de départ.

UPDATE content.core        SET walsn = pg_current_wal_lsn();
UPDATE commerce.product_core SET walsn = pg_current_wal_lsn();

COMMIT;

-- ==============================================================================
-- Vérification
-- ==============================================================================
SELECT
    table_schema || '.' || table_name AS table_fqn,
    column_name,
    data_type,
    column_default
FROM information_schema.columns
WHERE column_name = 'walsn'
  AND table_name IN ('core', 'product_core')
ORDER BY table_fqn;
