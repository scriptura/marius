# Document de travail — Trade‑offs et risques pour le projet Marius

## Extrait fondamental

> **« Le Core (`no_std`) de Marius n'est pas un espace de développement, c'est une cible de compilation. »**  
> **« La Forge lit ces intentions et produit le code d'exécution. »**

---

## 1. Résumé exécutif

**Marius** propose une séparation stricte entre _intention_ (SQL/DSL/DDL) et _exécution_ (Core `no_std` immuable) via une **Forge** centralisée qui génère le code AOT et documente les allocations. Ce modèle offre des invariants forts sur la performance et l’auditabilité mais concentre la complexité et le risque opérationnel dans l’outillage de génération.

---

## 2. Principaux trade‑offs (choix architecturaux et leurs conséquences)

- **Forge unique vs générateurs modulaires**
  - _Avantage :_ cohérence, invariants globaux, traçabilité.
  - _Inconvénient :_ point unique de défaillance, coût d’évolution élevé.
- **Core `no_std` immuable vs flexibilité développeur**
  - _Avantage :_ contrôle strict des hot paths, latence et empreinte mémoire prévisibles.
  - _Inconvénient :_ courbe d’entrée élevée pour développeurs Rust, interventions manuelles classées comme dette technique.
- **AOT pour la logique métier vs capacité d’expérimentation rapide**
  - _Avantage :_ code inspectable, pas de surprises runtime, optimisation CPU.
  - _Inconvénient :_ cycle de feedback plus long pour changements métier, nécessité d’outillage CI/forge robuste.
- **Zéro allocation sur hot path vs nécessité de `alloc` pour le web**
  - _Avantage :_ saturation cache L1/L2 et latence stable.
  - _Inconvénient :_ complexité pour prouver et documenter statiquement tous les points d’allocation.
- **Batching Collector/Dispatcher vs traitement immédiat d’événements**
  - _Avantage :_ protection contre l’amplification d’écriture et smoothing de charge.
  - _Inconvénient :_ latence d’actualisation accrue; complexité de backpressure et ordering.

---

## 3. Risques majeurs et impacts opérationnels

- **Risque central : défaillance de la Forge**
  - _Impact :_ blocage des livraisons, nécessité d’implémentations manuelles temporaires, perte de confiance dans le pipeline.
  - _Mesure d’atténuation :_ CI de génération, tests de régression de la Forge, versioning strict des générateurs.
- **Risque d’évolution métier rapide**
  - _Impact :_ multiplication des « contrats d’implémentation » et dette technique croissante.
  - _Mesure d’atténuation :_ prioriser extensibilité des générateurs, définir DSL expressif, backlog d’amélioration de la Forge.
- **Risque performance sous rafales massives**
  - _Impact :_ saturation CPU / I/O lors de mises à jour massives.
  - _Mesure d’atténuation :_ valider Collector/Dispatcher par tests de charge, prévoir files persistantes et stratégies de coalescing.
- **Risque sécurité et confinement**
  - _Impact :_ génération incorrecte de guards ou RLS mal traduits pouvant ouvrir des fuites.
  - _Mesure d’atténuation :_ Guard‑Forge avec proofs/tests, revue automatisée des traits générés.
- **Risque d’observabilité insuffisante**
  - _Impact :_ difficile d’identifier si le problème vient de l’intention, de la Forge ou du Core.
  - _Mesure d’atténuation :_ traces end‑to‑end, métriques sur génération, instrumentation du Collector/Dispatcher.
- **Risque humain et gouvernance**
  - _Impact :_ conflits sur qui peut modifier l’intention, mauvaise utilisation du DDL/DSL.
  - _Mesure d’atténuation :_ règles de contribution, revues d’Architecte de Forge, formation des utilisateurs de la Forge.

---

## 4. Mitigations techniques recommandées

- **Phase 0 : Prototype DB‑Forge**
  - Générer `#[repr(C)]` depuis SQL; valider symétrie mécanique et documentation d’allocation.
- **Tests de charge réalistes**
  - Scénarios : rafales 10k updates, procédures stockées massives, latence end‑to‑end. Mesurer CPU, IOPS, latence de flush.
- **Backpressure et persistance du Collector**
  - WAL‑backed queue ou journal pour garantir reprise et éviter perte d’événements.
- **CI/CD pour la Forge**
  - Tests unitaires de génération, fuzzing des DSL, contrats d’API pour le Core.
- **Observabilité et audits**
  - Traces distribuées, métriques sur allocations générées, rapports automatiques sur points d’allocation.
- **Workflow de « contrat d’implémentation »**
  - Template de trait généré, ticketing automatique, SLA pour l’Architecte de Forge.

---

## 5. Projets intéressants pour cas d’étude (sélection et pourquoi)

| **Projet**                 | **Pourquoi l’étudier**                                                                   | **Ce qu’il n’apporte pas à Marius**                                                         |
| -------------------------- | ---------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| **sqlc**                   | Génération AOT de structs/queries typées depuis SQL; pattern SQL→code.                   | Pas de contrainte `no_std` ni documentation automatique des allocations.                    |
| **Dapper.AOT**             | Exemple d’élimination de la réflexion/runtime par AOT; build‑time inspectable.           | Conçu pour .NET runtime; pas de modèle Forge centralisé `no_std`.                           |
| **sqlx (Rust)**            | Vérification compile‑time des requêtes et intégration async utile pour extraction/batch. | Usage runtime et dépendances `std` par défaut; nécessite adaptation pour hot path `no_std`. |
| **Debezium + Materialize** | Patterns CDC et vues matérialisées pour simuler flux et batching.                        | Systèmes runtime orientés streaming, pas AOT binaire.                                       |
| **Hasura / PostgREST**     | Exposition automatique d’API depuis Postgres; bonnes pratiques de mapping schéma→API.    | Moteurs runtime; pas de Core immuable `no_std`.                                             |
| **Prisma / ORM codegen**   | Génération de clients typés et workflows de migration; ergonomie développeur.            | Orienté ergonomie, pas invariants bas‑niveau mémoire.                                       |

---

## 6. Roadmap de conception minimale (livrables prioritaires)

1. **Prototype DB‑Forge** : SQL → `#[repr(C)]` + tests de symétrie et documentation d’allocation.
2. **Collector/Dispatcher PoC** : in‑memory dedupe + flush volumétrique et temporel + tests de rafale.
3. **Integration SQLx** : batch SELECT optimisés et orchestration Tokio pour extraction.
4. **Fragment‑Forge minimal** : génération de texte brut (AOT / `push_str`) pour un cas d’usage simple et validation multi‑threaded.
5. **CI/Observability** : pipelines de génération, métriques allocation, traces end‑to‑end.

---

## Conclusion

Le projet **Marius** offre une proposition technique puissante pour des systèmes où la **prévisibilité CPU**, la **traçabilité binaire** et la **discipline mémoire** sont critiques. Les risques principaux sont organisationnels et liés à la centralisation de la Forge, à la gestion des rafales d’écriture et à la gouvernance des contrats d’implémentation. Étudier **sqlc**, **Dapper.AOT**, **sqlx**, **Debezium/Materialize** et **Hasura** fournira des patterns concrets pour la génération AOT, la vérification SQL et la gestion de flux, mais il faudra concevoir explicitement les couches qui imposent `no_std`, la documentation statique des allocations et la gouvernance Forge‑centrée.

---
