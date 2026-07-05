**Rôle :** Ingénieur système Rust (AOT / DOD).

Nous poursuivons l'implémentation du Mode Page.
Nous suivons strictement la roadmap.

**Mission**

Implémenter exclusivement la **Phase 6.6**.

**Contraintes**

- ne modifier aucune fonctionnalité en dehors du périmètre de la Phase 6.6 ;
- ne préparer aucun comportement relevant des phases ultérieures ;
- n'introduire aucun `todo!`, `unimplemented!` ou code spéculatif ;
- respecter les invariants définis par les documents d'architecture ;
- ne modifier aucune signature publique de `fragment-forge`, sauf si la roadmap de cette phase le prévoit explicitement ;
- conserver le point de convergence unique sur `Vec<FlatPageToken<'src>>` ; aucune branche spécifique Mode Page n'est autorisée en aval de cette production.
- documenter exhaustivement les nouveaux invariants introduits par cette phase ;
- ajouter uniquement les tests prévus par la roadmap pour cette phase ;
- vérifier `cargo fmt`, `cargo test` et `cargo clippy`.

**À la fin de l'implémentation :**

- exécuter `cargo fmt --check`, `cargo test` et `cargo clippy --all-targets` ; si l'un d'eux échoue à cause d'un problème préexistant hors périmètre, l'identifier explicitement et confirmer qu'aucune régression n'a été introduite dans le diff de cette phase ;
- confirmer que le périmètre de la Phase 6.6 a été strictement respecté ;
- fournir le diff Git complet (pattern : `phase-6.6.diff`) ;
- fournir le rapport de fin de phase suivant le modèle en pièce jointe (pattern : `phase-6.6.md`) ;
- retourner le fichier `build.rs` en artefact autonome complet.

---

Liste des documents en pièce jointe :

- `architecture-pipeline-mode-page.md`
- `roadmap-mode-page-implementation.md`
- `doc2-linker-lowering.md`
- `end-of-phase report.md`
- `/forge/fragment-forge/src/lib.rs`

---

## Phase => Liste des documents à passer en pièce jointe :

- ✅ Phase 4.x => architecture + roadmap + doc1 + lib.rs + rapport end
- ✅ Phase 5.x => architecture + roadmap + doc2 + lib.rs + rapport end
- Phase 6.x => architecture + roadmap + doc1 + doc2 + doc3 + lib.rs + build.rs + rapport end

## Stem exact des fichiers :

- `architecture-pipeline-mode-page.md`
- `roadmap-mode-page-implementation.md`
- `doc1-parser-mode-page.md`
- `doc2-linker-lowering.md`
- `doc3-orchestration-build-rs.md`
- `end-of-phase report.md`
- `/forge/fragment-forge/src/lib.rs`
- `crates/core/schema/build.rs`

## Progression de la roadmap :

| Session     | Phases              | Pourquoi                                                                                             |
| ----------- | ------------------- | ---------------------------------------------------------------------------------------------------- |
| ✅ Terminée | 4.1 → 4.5           | Mise en place du langage du Parser (types + reconnaissance des principaux mots-clés).                |
| ✅ Terminée | **4.6 + 4.7**       | On termine entièrement le Parser : `extends` puis `Unsupported`. À la sortie, le Parser est complet. |
| ✅ Terminée | **5.1 + 5.2 + 5.3** | Début du Linker : découverte des blocs, collecte et validation structurelle.                         |
| ✅ Terminée | **5.4 + 5.5 + 5.6** | Correspondance des blocs, substitutions, préparation du lowering.                                    |
| ✅ Terminée | **5.7 + 5.8 + 5.9** | Lowering complet jusqu'à `Vec<FlatPageToken>`. À la sortie, l'IR canonique existe.                   |
| ✅ Terminée | **6.1 + 6.2 + 6.3** | Orchestration dans `build.rs` : détection, aiguillage, intégration du nouveau pipeline.              |
| 7           | **6.4 + 6.5 + 6.6** | Validation finale, nettoyage et preuve du diff nul sur les fonctions gelées.                         |
