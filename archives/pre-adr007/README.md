# Archive pré-ADR-007 — `body.rs`, `generator.rs`, `orchestrator.rs`, `prologue.rs`

Destination : `/marius/archives/pre-adr007/`

## État

Code désactivé. Retiré de `forge/fragment-forge/src/lib.rs` (suppression des
quatre déclarations `pub mod`). Ne compile plus au sein de la crate — conservé
tel quel, sans mise à jour, comme trace historique.

## Ce que c'était

Un pipeline de génération fonctionnel à quatre étages :
`orchestrator → generator → body → prologue`. Chaque module avait une
responsabilité textuelle claire (assembler, générer le corps, générer la
signature). Écriture d'un fichier Rust par table (`{table}_render.rs`),
fonction libre `fn render_page(...)`.

## Pourquoi c'est retiré

Pas pour un défaut de conception. Ce pipeline raisonnait à un niveau
d'abstraction que l'architecture a dépassé : génération de texte Rust, plutôt
que préservation d'invariants de layout mémoire. Le pipeline actif
(`scan → parse_tokens → validate_ast → resolve_and_measure →
generate_aot_snippet`, dans `lib.rs`) structure chaque étape autour d'une
représentation de données stable (`FlatPageToken`, `SchemaIndex`,
`TemplateMetrics`) — les quatre modules archivés n'ont jamais intégré cette
représentation.

Trois divergences concrètes relevées à l'audit, pour éviter de les
redécouvrir :

1. **`bool` natif vs `u8`-sentinelle.** `body.rs` émet
   `if record.field { }`. `StorageRow` est `#[repr(C)]` et contraint
   `bytemuck::Pod` (réinterprétation bit-à-bit sans invariant caché — exclut
   `bool`, dont seuls 0/1 sont des bit-patterns valides). Le pipeline actif
   émet `!= 0` sur u8 pour cette raison. **Ce point n'est pas propre à ce
   code abandonné** : la spécification v1.1 §8 (mode page) illustre encore
   `if record.field { }` — incohérence non résolue, à trancher
   indépendamment de cette archive avant toute implémentation du mode page.

2. **Capacité bakée en littéral vs expression compile-time.**
   `orchestrator.rs` écrit `pub const PAGE_STATIC_CAP: usize = 7;` — une
   valeur figée au moment du build, mesurée par une lecture disque
   indépendante de celle de `rustc`. La spécification (§5.3, changelog C-07)
   impose une expression (`static_partials::X.len() + …`) précisément pour
   éliminer ce risque de désynchronisation (CRLF/LF entre lecture `build.rs`
   et embarquement `include_str!`). Ne pas réintroduire ce pattern.

3. **Topologie fichier-par-table vs fichier unique.** `orchestrator.rs`
   écrit un fichier par table, constantes non préfixées. Le pipeline cible
   produit un seul `generated_schema.rs`, inclus via `include!()`,
   constantes préfixées par table. Les deux topologies sont incompatibles,
   pas seulement différentes en style.

## Usage attendu de cette archive

Référence historique uniquement. Ne pas réintégrer de code depuis ces
fichiers dans le pipeline actif, y compris pour l'implémentation du mode
page (`extends`/`block`) — chaque idée qu'ils portaient a déjà été tranchée
ailleurs, dans un sens incompatible avec eux. Voir handoff
`HANDOFF-mode-page-brique-structurelle.md` pour le travail en cours.
