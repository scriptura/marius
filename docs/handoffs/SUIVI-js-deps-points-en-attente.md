# Points en attente — session js_deps (chantiers 1 à 4 + Phase 2 walsn)

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

## 2. `js_deps` : 4 bits réservés, jamais câblés côté marqueurs

`scripts_registry.lock` porte `disclosure=1`, `map=2`, `media-player=4`,
`line-mark=8` — décidés en amont de cette session. Mais **aucun marqueur de
classe HTML ne leur est associé** dans `content.compute_js_deps()`
(`db/05_content/02_systems.sql`) : ces quatre capacités ne se déclencheront
jamais, quel que soit le contenu éditorial, tant que leur vocabulaire de
classes n'est pas fourni. Je ne l'ai pas inventé — voir le guide
`docs/guides/js-deps-capacites-frontend.md` pour la procédure d'ajout, à
appliquer telle quelle pour ces quatre-là quand leur vocabulaire sera
disponible.

De même, `theme.toml [scripts.capabilities]` ne porte à ce jour que
`image-focus` confirmé avec un chemin de fichier réel. `range`/`youtube-embed`
ont leurs bits (`32`/`64`) et leurs marqueurs SQL déjà câblés, mais leurs
sections `theme.toml` (`entry`/`activation`) restent à écrire avec les vrais
chemins de fichiers JS — je n'ai pas de confirmation que `range.js`/
`youtube.js` existent réellement sous `assets/default/scripts/development/`.

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

## 6. Phase 2 `walsn` — un angle non entièrement vérifié

`store_registry.rs` (générique, `P::Record: Pod`) et les fichiers du
pipeline `pack.bin` (`batch_renderer.rs`/`pack_html_index.rs`/
`pack_html_format.rs`) ont été vérifiés : aucun ne suppose une
représentation particulière de `walsn`, tout fonctionne avec le nouveau
`u64`. Mais je n'ai jamais vu l'intégralité de `crates/shell/render` (le
crate est bien plus large que les fichiers fournis) — si un consommateur de
`walsn` existe ailleurs, encore non identifié, avec une attente différente
(offset dans une région `mmap` spécifique, ordre d'octets), il faudrait le
vérifier séparément. Probabilité jugée faible (le mécanisme réel repose sur
`content_core_walsn`/`trg_content_core_notify`, tous deux déjà couverts),
mais pas une certitude absolue.
