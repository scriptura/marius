# Phase 4.4 — Reconnaissance `{% block %}` / `{% endblock %}`

**Diff : une seule fonction modifiée** — `parse_page_block`, désormais `Result<PageSourceToken<'src>, PageComposeParseError>` au lieu de `Result<FlatPageToken<'src>, ...>`. Le dispatch de tête (`parse_page_tokens`) propage directement sa sortie sans ré-envelopper (`if`/`endif` → `Runtime`, `block`/`endblock` → `Block`, décision prise en un seul point désormais). Zéro nouvelle branche `match` sur `SpanKind`, zéro fonction ajoutée — conforme au périmètre roadmap §4.4.

**Invariant introduit** : `{% block name %}` → `Block(BlockOpen { name })`, `{% endblock %}` → `Block(BlockEnd)`, sans pile d'état, sans vérification d'appariement ni de nom à la fermeture — permissivité actée Document 1 §4/§6. Documenté inline et prouvé par `nested_blocks_parse_succeeds` (des blocs imbriqués passent, la Validation Document 2 tranchera plus tard).

**Tests ajoutés** (les deux prévus par la roadmap) :

- `single_top_level_block_produces_block_open_and_block_end`
- `nested_blocks_parse_succeeds`

**Vérifications**

- `cargo test` : **41/41 verts** (39 → 41, zéro régression sur 4.1–4.3).
- `cargo fmt --check` : mon diff est fmt-clean (le seul écart introduit a été corrigé). Le fichier garde 103 diffs préexistants dans la baseline (style d'alignement volontaire hors périmètre 4.4, non modifié — je n'ai pas appliqué `cargo fmt` en aveugle pour ne pas reformater du code hors scope).
- `cargo clippy --all-targets` : **0 nouveau warning**. Le seul warning présent (`needless_lifetimes` sur `generate_aot_snippet`, ligne 1740) préexiste à l'identique dans la baseline — confirmé par diff avant/après.

**Périmètre strictement respecté** : aucun `todo!`/`unimplemented!`, aucune anticipation de 4.5 (`static`)/4.6 (`extends`)/4.7 (`Unsupported`) — ces mots-clés continuent d'échouer via `InvalidBlockSequence`, comportement inchangé. J'ai mis à jour les commentaires de portée devenus factuellement faux (4.3, `InvalidBlockSequence`) pour refléter que `block`/`endblock` sont sortis du catch-all — documentation, pas fonctionnalité.

Les deux fichiers livrés : le diff Git complet et le `lib.rs` final.
