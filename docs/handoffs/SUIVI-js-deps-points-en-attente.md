# Points en attente — note de suivit

Note de suivi, pas un handoff de reprise de travail — chaque point ci-dessous
est soit une décision explicitement différée, soit une découverte faite en
cours de session sans être corrigée. Aucun n'empêche l'état actuel de
fonctionner (`content.core` seul, `cargo build`/`test`/`clippy` verts,
`marius-dump`/`marius` opérationnels).

## 1. Routes désactivées — priorité basse, actée

`commerce.product_core` retiré de `ROUTE_TABLE`/`SHARDS`/`main.rs` (session
en cours). Fonctionnel jusqu'ici par accident (jamais retouché depuis une
première PoC naïve), jamais un choix délibéré de couverture — la PoC actuelle
se concentre sur `content.core`.

Pour le réactiver un jour :
- Remettre les entrées `ROUTE_TABLE`/`SHARDS`/Dispatcher/canal `LISTEN` dans
  `crates/shell/server/src/main.rs` (retiré proprement, facile à réintroduire
  par symétrie avec `content_core`).
- **`commerce.product_core` n'a jamais eu de mécanisme de dump.** `dump.rs`
  est câblé exclusivement pour `content_core` (`ContentCoreProjection` codé
  en dur, aucune boucle générique sur les projections). Il faudra soit
  l'étendre, soit créer un second binaire, avant de pouvoir régénérer son
  `store.bin`.
- `pages_homepage` (troisième entrée de l'ancien `ROUTE_TABLE`) n'a jamais
  été creusée cette session — état inconnu, à vérifier séparément.

## 3. Duplication de logique de conversion NOT NULL (db-forge)

Découverte en corrigeant `walsn` : `codegen/from_impl.rs`
(`write_from_impl`) et `codegen/projection.rs` (`write_projection_stub`,
bloc `fetch_from_pg` pour les composants à jointure varlena) portent chacun
**leur propre copie** du `match m.row_type { ... }` gérant les conversions
`NOT NULL` (types `chrono`, désormais `pg_lsn`). J'ai corrigé les deux
séparément — la seconde n'a été découverte qu'après coup, via une erreur de
compilation. Si une future colonne a encore besoin d'un traitement spécial
(un nouveau `select_cast`, ou toute autre conversion `NOT NULL` non
triviale), il faudra penser aux **deux** sites, pas un seul. Candidat
naturel à factoriser un jour (fonction commune appelée par les deux
générateurs), non fait ici — perimètre non demandé.

## 4. Étiquetage trompeur de `v_master_health_audit.sql`

`triage_status = 'CRITICAL (SECURITY BREACH)'` se déclenche dès
`debt_score >= 100`, quelle qu'en soit la cause (cumul de plusieurs alertes
indépendantes) — pas nécessairement une brèche de sécurité réelle
(`security_breach_alert` peut valoir `false` sur la même ligne). Repéré sur
`content.core` en cours de session (avant la correction Phase 2 walsn,
alerte alors due au cumul `hot_blocker_alert` + `density_drift_alert` +
`bloat_alert`). Proposition faite, jamais tranchée : renommer le palier ou
distinguer le libellé de la cause réelle. Pas touché — préexistant à cette
session, sans lien avec `js_deps`.

## 5. `hot_blocker_alert` sur `content.core` — préexistant, non traité

`published_at`, `author_entity_id`, `modified_at` sont indexés et absents
d'`immutable_keys` — structurel, vrai indépendamment de `js_deps`, présent
avant toute intervention de cette session. Signalé, jamais corrigé (hors
périmètre demandé).

