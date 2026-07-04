# Rapport de fin de phase — 6.1 : `read_template_file` (extraction pure)

## 0. Avertissement de vérification

Cet environnement ne dispose pas de toolchain Rust (`cargo`/`rustc` absents, `crates/core/schema` complet — dépendances `marius_db_forge`, `marius_fragment_forge`, `sqlx`, `tokio` — non fourni). Je n'ai donc **pas pu exécuter réellement** `cargo fmt`, `cargo test`, `cargo clippy` ni le jalon vert de la Phase 6.1 (diff octet-à-octet de `generated_schema.rs`). Ce qui suit est une revue manuelle stricte du diff, pas une confirmation d'exécution CI. À faire de votre côté avant merge :
```
cargo build -p schema   # capturer generated_schema.rs avant
git apply phase-6.1.diff
cargo build -p schema   # comparer generated_schema.rs après (diff attendu : nul)
cargo fmt --check -p schema
cargo clippy -p schema
cargo test -p schema
```

## 1. Livrables

- **Tests ajoutés :** aucun. Conforme à la roadmap 6.1 : *« aucun nouveau test unitaire nécessaire — le jalon est un test de non-régression au niveau build »*. Le jalon vert attendu est un diff nul sur `generated_schema.rs` avant/après (à exécuter côté utilisateur, cf. §0).
- **Code :** extraction de `fn read_template_file(path: &Path) -> Result<String, ()>` depuis le corps de `resolve_template`, signature strictement conforme à celle gelée par le Document 3 (§4).

## 2. Analyse architecturale de la phase

- **Invariant introduit :** la lecture brute d'un fichier `.marius` est isolée dans une fonction pure (une seule responsabilité : I/O de lecture + traduction d'erreur), indépendante du contexte `schema`/`table` de l'appelant. C'est cet invariant qui rend la fonction réutilisable telle quelle pour un second fichier (parent, Phase 6.3) sans modification de signature.
- **Invariants existants confirmés :** toute l'I/O disque du pipeline Voie B reste concentrée dans `build.rs` (aucune E/S n'a migré vers `marius_fragment_forge` ou `marius_db_forge`) ; la signature externe de `resolve_template` est inchangée ; le chemin Mode Fragment (`scan → parse_tokens → validate_ast → resolve_and_measure → generate_aot_snippet`) n'a subi aucune modification de corps en dehors du point d'appel à la lecture.
- **Invariants devenus inutiles ou faux :** aucun. La phase ne retire ni ne contredit rien d'existant — refactor pur, aucune fonction gelée touchée.
- **Mesures réelles obtenues :** aucune (`size_of`, benchmark) — hors périmètre d'un refactor d'I/O sans structure de données nouvelle. Diff produit : +18 lignes / −3 lignes, une seule fonction ajoutée, un seul point d'appel modifié (cf. `phase-6.1.diff`).
- **Hypothèses des documents confirmées/infirmées :** confirmée — le Document 3 §4 anticipait exactement cette signature (`fn read_template_file(path: &Path) -> Result<String, ()>`) et le found code de `resolve_template` correspondait effectivement au bloc `std::fs::read_to_string(...).map_err(...)` déjà présent, sans écart entre la spec et le code réel sur ce point précis.

**Écart mineur assumé, à signaler explicitement :** le message `cargo:error` perd le contexte `[{schema}.{table}]` (la fonction extraite n'a plus accès à ces paramètres, absents de la signature gelée) ; il est remplacé par le chemin complet du fichier (`path.display()`), qui est au moins aussi identifiant. Ceci n'affecte pas le contenu de `generated_schema.rs` (le message n'apparaît que sur le chemin d'échec, qui interrompt le build avant toute écriture de fichier) — le jalon octet-à-octet reste valide. Aucun test existant ne dépend du texte exact de ce message (vérifié dans les fichiers fournis).

## 3. Impact documentaire

- **Documentation obsolète :** aucune — le Document 3 décrivait déjà `read_template_file` comme une fonction à créer ; son existence dans le code la rend désormais *effective* plutôt que *prévue*, sans contradiction.
- **À corriger maintenant :** rien de bloquant. Point de vigilance mineur pour une synthèse future : le Document 3 (§4) ne précisait pas explicitlement si le message d'erreur devait conserver `schema`/`table` ou non — c'est tranché ici (non, par contrainte de signature), à noter si une ADR de clôture du Mode Page est rédigée.
- **À régénérer en fin d'implémentation complète :** le tableau du Document 3 §3 (« Responsabilités par étape ») reste correct tel quel — `read_template_file` y figure déjà comme « lecture parent, Oui — seule E/S nouvelle du build » ; rien à changer à ce stade, seulement à confirmer en synthèse finale que l'implémentation a suivi ce tableau sans écart.

## 4. Impact sur la roadmap

- **Prochaines phases toujours pertinentes :** oui, 6.2 → 6.6 restent inchangées dans leur découpage. Aucune fusion ni découpage supplémentaire suggéré par cette phase — elle est trop petite (un refactor d'extraction) pour révéler un besoin de renegociation du séquencement.
- **Risques disparus :** le risque « signature de `read_template_file` à définir a posteriori en 6.3, potentiellement incompatible avec l'usage 6.1 » disparaît — la fonction est déjà au format final, 6.3 n'aura qu'à l'appeler une seconde fois sur `parent_path`.
- **Nouveaux risques :** aucun nouveau risque structurel. Point de vigilance opérationnel seulement : si un test externe (hors périmètre des fichiers fournis) vérifiait le texte exact `cargo:error=DB-Forge [{schema}.{table}] : lecture du template échouée`, il casserait sur ce message générique — à vérifier côté CI avant merge.
- **Signatures simplifiées / structures inutiles :** sans objet à ce stade (aucune structure de données touchée en 6.1).
- **Implémentation plus élégante que documentée :** non — le diff correspond exactement à ce que le Document 3 §4 et la roadmap 6.1 prescrivaient, sans marge d'amélioration identifiée qui justifierait un écart.

## 5. Regard d'architecte

Aucune propriété nouvelle non anticipée par les documents. C'est le résultat attendu d'un refactor pur classé comme tel par la roadmap elle-même : la phase confirme une prévision plutôt qu'elle n'en révèle une. Le seul point à conserver pour la synthèse finale (pas d'ADR nécessaire, trop mineur) est l'écart de message d'erreur documenté en §2 — pure conséquence mécanique de la contrainte de signature déjà actée, non une découverte architecturale.
