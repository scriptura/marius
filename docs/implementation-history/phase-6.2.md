# Rapport de fin de phase — 6.2 : branchement `detect_extends` (sans câblage aval)

## 0. Avertissement de vérification

Comme signalé en Phase 6.1 : pas de toolchain Rust ni crate complet ici (`marius_db_forge`, `marius_fragment_forge` compilés, `sqlx`, connexion `DATABASE_URL`). `cargo fmt --check`, `cargo test`, `cargo clippy --all-targets` **non exécutés réellement**. Revue manuelle uniquement, détaillée ci-dessous.

**Point non résolu, à traiter par vous avant merge :** le test de build prescrit par la roadmap (« fixture `.marius` commençant par `{% extends %}` → build échoue avec le message attendu ») exige une table réellement enregistrée dans le registry (`fetch_component_list`, base `marius_admin`) pointant vers ce fichier fixture — `resolve_template` n'est jamais invoquée pour un fichier qui n'est pas rattaché à un composant du registry. Je n'ai pas visibilité sur ce registry ni sur un éventuel harnais de test d'intégration existant pour `build.rs` (aucun répertoire de fixtures fourni). Je n'ai donc **pas ajouté ce test** pour éviter de fabriquer une fixture qui ne correspondrait pas à l'infrastructure réelle. Voir §1 pour la procédure de vérification manuelle en attendant.

## 1. Livrables

- **Code :** un point de branchement ajouté dans `resolve_template`, juste après `read_template_file` (6.1) et avant `scan(&src)` : appel à `detect_extends(&src)` (déjà `pub` dans `marius_fragment_forge`, Phase 4.2, aucune modification de signature) ; branche vraie → `cargo:error` + `Err(())`.
- **Tests ajoutés :** aucun de mon côté (cf. §0). À exécuter manuellement :
  1. Fixture négative — créer `templates/{schema}/{table}.marius` commençant par `{% extends "..." %}` pour un composant du registry, lancer `cargo build -p schema`, vérifier échec avec exactement `cargo:error=DB-Forge [{schema}.{table}] : Mode Page non câblé` et code de sortie 1.
  2. Fixtures existantes — `cargo build -p schema` sur l'état actuel du registry (aucun template n'utilise `extends` aujourd'hui, cf. Document 3 architecture-pipeline §8 : *"Tout fichier `.marius` trouvé est traité comme un fragment"*) → build vert, chemin Mode Fragment inchangé. Confirmé par lecture du diff : aucune ligne du chemin Fragment (`scan → parse_tokens → validate_ast → resolve_and_measure → generate_aot_snippet`) n'est touchée, la nouvelle condition est un embranchement strictement avant ce chemin, donc structurellement incapable de l'affecter pour tout fichier où `detect_extends` retourne `false`.

## 2. Analyse architecturale de la phase

- **Invariant introduit :** le point de décision de mode est unique dans tout le fichier — un seul appel à `detect_extends`, à un seul endroit (`resolve_template`, juste après lecture). Aucune autre position du fichier ne teste `extends`. Conforme au Document 3 §1 : *"Aucune autre position n'est acceptable : toute lecture ultérieure (scan, parse) présuppose déjà un choix de grammaire."*
- **Invariants existants confirmés :** signature externe de `resolve_template` inchangée ; `validate_ast`/`resolve_and_measure`/`generate_aot_snippet` non touchées (gelées, Document 3 §7) ; toute l'E/S disque reste concentrée dans `build.rs` (`detect_extends` n'en fait aucune — confirmé par sa doc dans `lib.rs` : *"Aucune E/S : pas d'appel à `std::fs`"*).
- **Invariants devenus inutiles ou faux :** aucun.
- **Mesures réelles obtenues :** aucune — la phase n'introduit aucune structure de données, seulement un branchement conditionnel sur un `bool` déjà calculé par une fonction gelée. Diff : +12/−2 lignes (import + branche + doc), une seule fonction modifiée.
- **Hypothèses des documents confirmées/infirmées :** confirmée — le Document 3 §1 anticipait exactement ce point d'insertion (« immédiatement après cette lecture, avant le premier appel à `scan()` »), et le code réel de `resolve_template` (post-6.1) correspondait bien à cette description sans écart.

**Décision d'implémentation à documenter explicitement (écart mineur vis-à-vis de la formulation littérale de la roadmap) :** la roadmap 6.2 décrit l'effet observable comme *« cargo:error + exit(1) »*. Le code n'appelle pas `std::process::exit(1)` directement dans `resolve_template` — il émet `cargo:error` puis retourne `Err(())`, exactement comme tous les autres chemins d'erreur existants de cette fonction (parsing, validation, résolution). C'est `main()` qui appelle déjà `std::process::exit(1)` sur tout `Err(())` reçu de `resolve_template` (`.unwrap_or_else(|()| { std::process::exit(1); })`, code gelé, non touché). L'effet externe observable (cargo:error émis, process quitte avec code 1) est strictement identique ; la forme interne choisie préserve la cohérence totale avec les 3 autres branches d'erreur déjà existantes dans cette même fonction, ce qui semble préférable à l'introduction d'un unique appel direct à `exit(1)` incohérent avec le reste du corps de la fonction.

## 3. Impact documentaire

- **Documentation obsolète :** le point 5 de l'architecture-pipeline (« API Mode Page non implémentée, non détectée par l'orchestrateur ») devient partiellement obsolète : `build.rs` détecte désormais le mode (branchement présent), même s'il refuse encore de le traiter. Formulation à corriger dans une synthèse ultérieure : passer de « aucun branchement de détection de mode » à « branchement de détection présent, câblage aval non fait (refus explicite) ».
- **À corriger maintenant :** rien de bloquant pour poursuivre 6.3.
- **À régénérer en fin d'implémentation complète :** Document 3 §8 (l'orchestrateur), une fois le câblage complet (6.6), pour remplacer la description du refus explicite par la description du chemin Mode Page réellement câblé.

## 4. Impact sur la roadmap

- **Prochaines phases toujours pertinentes :** oui. 6.3 s'enchaîne directement sur ce point de branchement (remplacer le `return Err(())` par l'appel à `resolve_page_template`), sans renegociation de découpage nécessaire.
- **Fusions/découpages :** aucun changement suggéré. La granularité 6.1/6.2 séparée s'est révélée juste : 6.1 (refactor pur, zéro risque) et 6.2 (un branchement, risque isolé et testable indépendamment) restent deux jalons vérifiables séparément, ce qui aurait été perdu en les fusionnant.
- **Risques disparus :** le risque « où insérer le point de détection sans dupliquer une lecture ou un `scan` » disparaît — le point d'insertion est posé et validé manuellement contre le Document 3 §1.
- **Nouveaux risques :** un point de vigilance opérationnel, pas structurel — signalé en §0 : le test de build prescrit par la roadmap dépend d'une infrastructure (registry DB + fixture) que je n'ai pas pu vérifier. Risque que ce test soit oublié si le harnais de test build-level n'existe pas encore dans le projet — à confirmer avant de considérer 6.2 réellement close au sens du jalon vert de la roadmap.
- **Signatures simplifiées / structures inutiles :** sans objet.
- **Implémentation plus élégante :** non — le point d'insertion et la forme du refus (cohérente avec les branches d'erreur existantes plutôt qu'un `exit(1)` direct, cf. §2) suivent fidèlement l'intention du Document 3, sans écart méritant d'être noté comme amélioration.

## 5. Regard d'architecte

Rien de nouveau non anticipé par les documents. Un point mérite néanmoins d'être conservé pour la synthèse finale (pas une ADR, trop mineur pour ce niveau) : la roadmap décrit l'effet de bord au niveau du *process* (« exit(1) ») là où le code raisonne au niveau de la *fonction* (`Result<_, ()>`) et délègue la terminaison du process à l'appelant unique (`main`). Cette phase confirme que cette discipline (aucune fonction interne à `build.rs` autre que `main` n'appelle jamais `exit` directement) tient déjà pour les 4 branches d'erreur de `resolve_template` — invariant implicite du fichier, jamais nommé explicitement dans les documents, qui mériterait une ligne dans une synthèse finale d'architecture si une ADR de clôture du Mode Page est rédigée.
