**Rôle :** Ingénieur système Rust (AOT / DOD).

Nous poursuivons l'implémentation du Mode Page.
Nous suivons strictement la roadmap.

**Mission**

Implémenter exclusivement la **Phase 4.5**.

**Contraintes**

- ne modifier aucune fonctionnalité en dehors du périmètre de la Phase 4.5 ;
- ne préparer aucun comportement relevant des phases ultérieures ;
- n'introduire aucun `todo!`, `unimplemented!` ou code spéculatif ;
- respecter les invariants définis par les documents d'architecture ;
- documenter exhaustivement les nouveaux invariants introduits par cette phase ;
- ajouter uniquement les tests prévus par la roadmap pour cette phase ;
- vérifier `cargo fmt`, `cargo test` et `cargo clippy`.

**À la fin de la session, fournir :**

- le diff Git complet ;
- confirmation au VERT de `cargo fmt`, `cargo test` et `cargo clippy` ;
- la confirmation que le périmètre de la Phase 4.5 a été strictement respecté ;
- le rapport de fin de phase suivant le modèle convenu.

---

## Phase => Liste des documents à passer en pièce jointe :

Phase 4.x => architecture + roadmap + doc1 + rapport end phase + lib.rs
Phase 5.x => architecture + roadmap + doc2 + rapport end phase + lib.rs
Phase 6.x => architecture + roadmap + doc3 + rapport end phase + lib.rs

## Stem exact des fichiers :

- `architecture-pipeline-mode-page.md`
- `roadmap-mode-page-implementation.md`
- `doc1-parser-mode-page.md`
- `doc2-linker-lowering.md`
- `doc3-orchestration-build-rs.md`
- `end-of-phase report.md`
- `/forge/fragment-forge/src/lib.rs`
