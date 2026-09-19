# Handoff — Cartographie Hyper/Axum pour le pipeline Runtime de Segments

**Contexte de session précédente :** série de délibérations ADR-011 (avec GPT/Gemini) ayant abouti à deux documents stables, joints ci-dessous. Cette nouvelle session ouvre le premier des trois chantiers laissés hors périmètre du DESIGN.

**Objectif de cette session :** cartographier où l'architecture Runtime de segments (chaîne `SegmentDescriptor[] → MaterializedSource[] → EmissionPlan → IoSlice[] → backend`) rencontre les abstractions Hyper/Axum. Déterminer :

- quelles garanties Hyper conserve jusqu'au socket ;
- où apparaissent les copies éventuelles ;
- si `IoSlice` peut être propagé jusqu'au syscall ;
- si `writev`/`sendmsg` peuvent être utilisés sans détourner Hyper ;
- dans quels cas `hyper::upgrade` devient réellement nécessaire.

Livrable attendu : cartographie technique des possibilités, puis recommandation argumentée. Aucune solution privilégiée a priori.

---

## Fichiers à fournir en contexte (Markdown + TOML)

**Documents de décision (produits en session précédente) :**
- `DESIGN-runtime-segment-pipeline.md` — le document de référence (statut Accepté) sur lequel toute la cartographie doit s'articuler.
- `ADR-011-projections-ordonnancees.md` (révisée) — doctrine, invariants de capacité, périmètre restreint vis-à-vis d'ADR-008.
- `ADR-006-amendement-statut.md` — périmètre restreint d'ADR-006 (sendfile toujours valide pour le cas sans volatil).

**Documents de contexte projet, non lus ou partiellement lus cette session :**
- `Cargo.toml` du workspace, et celui du crate render-shell en particulier — première chose à vérifier : version exacte de `hyper` (0.14 vs 1.x, API très différente) et d'`axum`.
- `docs/rust/specifications/orchestration-main-specification.md` — probablement le point de bootstrap du serveur ; détermine si l'accès est déjà à un niveau bas (`hyper::server::conn`) ou entièrement dans l'abstraction `Service`/`Router` d'Axum.

**Déjà lus en session précédente, à rejoindre si utile pour éviter une redemande :**
- `marius-render-shell-specification.md` — `PackfileEntry`, `ROUTE_TABLE`, `LiveRegistry`, Option A (`read_at`→`Vec<u8>`) vs Option B jamais engagée.
- `runtime-lifecycle-guide.md`, `db-forge-specification.md` — cycles d'invalidation, `ComponentConfig`/trait `Projection` existant.
- `DESIGN-store-registry.md` — patron `Arc`/`ArcSwap` déjà retenu ailleurs, réutilisé par invariant (pas par mécanisme) dans `DESIGN-runtime-segment-pipeline.md` §3.

**Fichiers `.rs` :** volontairement différés — à demander seulement une fois la version de Hyper/Axum connue et le point de bootstrap identifié (`handlers.rs` du crate render-shell sera probablement le premier nécessaire, pour voir le type de `Body` actuellement utilisé dans la `Response`).

---

## Note pour mémoire — deux chantiers restants après celui-ci

1. **Devenir du trait `Projection` historique** (ADR-011 §3). Le trait actuel fusionne les niveaux 1 et 2 de l'ontologie (extraction de données + génération + écriture d'artefact), 1:1 avec une table SQL — nommage historique, antérieur à la clarification Projection/Artefact/Segment. Pur nettoyage conceptuel selon GPT et Gemini ; sans impact sur les invariants du DESIGN. Non commencé.

2. **Mesure `MSG_ZEROCOPY`** (DESIGN §9.5). Phase 1 retient `writev` sans `MSG_ZEROCOPY` — copie noyau résiduelle acceptée comme coût connu. Activation possible plus tard, mais seulement après un banc de mesure réel (coût de notification `MSG_ERRQUEUE`, seuil de taille de segment en dessous duquel `MSG_ZEROCOPY` devient contre-productif) — jamais par anticipation, même discipline que celle déjà appliquée par ADR-007/ADR-008. Non commencé, aucun chiffrage disponible.
