**Rôle :** Ingénieur système Rust (AOT / DOD).

Nous poursuivons l'implémentation du Mode Page.
Nous suivons strictement la roadmap.

**Mission**

Implémenter exclusivement la **Phase 5.1**.

**Contraintes**

- ne modifier aucune fonctionnalité en dehors du périmètre de la Phase 5.1 ;
- ne préparer aucun comportement relevant des phases ultérieures ;
- n'introduire aucun `todo!`, `unimplemented!` ou code spéculatif ;
- respecter les invariants définis par les documents d'architecture ;
- documenter exhaustivement les nouveaux invariants introduits par cette phase ;
- ajouter uniquement les tests prévus par la roadmap pour cette phase ;
- vérifier `cargo fmt`, `cargo test` et `cargo clippy`.

**À la fin de la session, fournir :**

- le diff Git complet ;
- confirmation au VERT de `cargo fmt`, `cargo test` et `cargo clippy` ;
- la confirmation que le périmètre de la Phase 5.1 a été strictement respecté ;
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

## Progression de la roadmap :

| Session     | Phases              | Pourquoi                                                                                                       |
| ----------- | ------------------- | -------------------------------------------------------------------------------------------------------------- |
| ✅ Terminée | 4.1 → 4.5           | Mise en place du langage du Parser (types + reconnaissance des principaux mots-clés).                          |
| ✅ Terminée | **4.6 + 4.7**       | On termine entièrement le Parser : `extends` puis `Unsupported`. À la sortie, le Parser est complet.           |
| 3           | **5.1 + 5.2 + 5.3** | Début du Linker : découverte des blocs, collecte et validation structurelle. Ces trois phases forment un tout. |
| 4           | **5.4 + 5.5 + 5.6** | Correspondance des blocs, substitutions, préparation du lowering.                                              |
| 5           | **5.7 + 5.8 + 5.9** | Lowering complet jusqu'à `Vec<FlatPageToken>`. À la sortie, l'IR canonique existe.                             |
| 6           | **6.1 + 6.2 + 6.3** | Orchestration dans `build.rs` : détection, aiguillage, intégration du nouveau pipeline.                        |
| 7           | **6.4 + 6.5 + 6.6** | Validation finale, nettoyage et preuve du diff nul sur les fonctions gelées.                                   |
