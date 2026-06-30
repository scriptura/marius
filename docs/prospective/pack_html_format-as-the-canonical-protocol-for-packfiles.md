# Prospective — `pack_html_format` comme protocole canonique des Packfiles

## Contexte

Les travaux menés autour du provisioning idempotent ont mis en évidence un point architectural intéressant.

L'intuition initiale consistait à réutiliser directement `apply_merge_io_sync()` comme unique primitive d'écriture des packfiles. L'analyse détaillée a montré que cette fonction appartient en réalité au pipeline de régénération : elle suppose l'existence d'une génération précédente (`PackHtmlIndex`) et résout le problème « produire une nouvelle génération à partir d'une ancienne ».

Le provisioning répond à une autre problématique : créer une première génération valide lorsqu'aucun packfile n'existe encore.

La véritable primitive commune se situe un niveau plus bas.

## Constat

`pack_html_format.rs` apparaît progressivement comme la véritable source de vérité du format physique des packfiles.

Ce module ne dépend ni :

- du Dispatcher,
- du LiveRegistry,
- du pipeline de régénération,
- de Tokio,
- ni de PostgreSQL.

Il décrit uniquement le contrat binaire du format :

- structure des entrées (`PackfileEntry`) ;
- structure du footer (`PackfileFooter`) ;
- règles d'alignement ;
- versionnement ;
- sérialisation.

Autrement dit, il ne décrit pas **comment** un packfile est produit, mais **ce qu'est** un packfile valide.

Cette distinction est fondamentale.

## Vision

À terme, `pack_html_format` pourrait être considéré non plus comme un simple module utilitaire mais comme une véritable spécification exécutable du format Packfile.

Tous les composants manipulant un packfile devraient idéalement dépendre exclusivement de cette définition commune.

Par exemple :

- le provisioning idempotent ;
- le moteur de rendu ;
- le lecteur (`PackHtmlIndex`) ;
- les futurs outils de diagnostic ;
- un éventuel inspecteur de packfiles ;
- un validateur de cohérence ;
- un compacteur ;
- un exporteur ou convertisseur.

Le format deviendrait alors un protocole interne clairement identifié de l'architecture Marius.

## Invariant recherché

Une seule définition décrit le format physique des packfiles.

Les différentes parties du système ne partagent jamais de logique métier, mais uniquement ce contrat.

Chaque couche conserve ensuite sa responsabilité propre :

- `pack_html_format` définit le format.
- `PackHtmlIndex` interprète ce format.
- `apply_merge_io_sync` produit une nouvelle génération.
- `LiveRegistry` publie atomiquement cette génération.
- Le Dispatcher orchestre le pipeline réactif.

Aucune de ces couches ne devrait absorber la responsabilité d'une autre.

## Conséquence potentielle

Si l'écosystème Marius s'enrichit d'outils satellites, ceux-ci ne devraient pas dépendre du moteur de projection lui-même.

Ils devraient uniquement parler le « langage Packfile ».

Cette approche permettrait de faire du format un véritable contrat d'architecture, stable, indépendant du pipeline de projection et réutilisable par tout outil manipulant les artefacts produits par Marius.

## Statut

Cette réflexion ne nécessite aucun changement immédiat.

Elle constitue une direction architecturale à réévaluer lorsque l'écosystème Marius disposera de plusieurs outils manipulant directement les packfiles.

---

_note rédigée le 30 juin 2026_
