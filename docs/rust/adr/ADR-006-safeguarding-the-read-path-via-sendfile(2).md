# ADR-006 : Sanctuarisation du Read Path via sendfile(2) et Éradication de la VM Applicative

## Statut

Accepté

## Contexte

La spécification v0.2.1 introduisait une Machine Virtuelle (VM) à 6 opcodes exécutant des opérations de copie réseau à partir d'arènes binaires dynamiques partagées avec PostgreSQL. L'audit rigoureux de cette architecture a révélé des vulnérabilités et des rigidités systémiques majeures :

- Un couplage bilatéral destructeur entre le layout physique de la base de données et les offsets d'opcodes compilés en Ahead-Of-Time (AOT), risquant des corruptions de mémoire silencieuses en cas de divergence de schéma.
- L'absence de gestion déterministe des débordements de mémoire (_buffer overreads_) et de la contre-pression réseau (_backpressure_) au sein de la VM applicative.
- Une complexité accidentelle élevée contredisant l'objectif d'un pipeline de rendu minimaliste, prédictible et robuste pour la version v1 du moteur Marius.

Par ailleurs, la spécification dépréciée v0.1 d'Inversion de Projection Statique tentait de gouverner le layout binaire de la base de données directement par la vue, introduisant une instabilité structurelle intenable face aux évolutions fonctionnelles.

## Décision

Pour purifier l'architecture, éliminer la dette technique d'exécution et garantir la livrabilité immédiate de Marius v1, les arbitrages suivants sont arrêtés :

1. **Éradication de la couche d'exécution intermédiaire :** Suppression définitive de la VM à opcodes et des arènes binaires dynamiques au runtime.

2. **Sanctuarisation du Read Path ($O(1)$ Kernel) :** Le chemin de lecture est intégralement délégué à l'appel système Linux `sendfile(2)`. Le moteur applicatif Rust ne manipule plus aucun octet à la lecture.

3. **Authentification et Sécurité AOT :** Les politiques d'accès (Row-Level Security / `auth_bits`) sont évaluées exclusivement par PostgreSQL de manière préemptive lors des transactions d'écriture (Write Path). L'existence physique de l'artéfact HTML sur le disque conditionne le droit d'accès. Le routeur Axum se limite à extraire l'identité pour mapper le chemin VFS (ex: `/artifacts/{role_id}/{entity_id}.html`) et invoque `sendfile(2)`. En cas d'absence du fichier, le noyau renvoie une erreur immédiatement traduite en `404 Not Found`.

4. **Préservation des Invariants Matériels :**

- **AOT Statique :** Les templates `.marius` sont convertis au build-time (via `build.rs`) en code Rust natif utilisant des appels directs à `push_str` et `write_fmt`.
- **Normalisation d'Indexation :** Les configurations externes humaines rédigées en base-1 sont systématiquement converties en base-0 à la compilation pour la gestion des structures et offsets internes.
- **Séparation Taxonomique :** L'arborescence des répertoires de stockage documente la taxonomie métier mais ne pilote ni ne déclenche aucun algorithme de calcul ou de routage dynamique au runtime.

## Conséquences

- **Performances Réseau Maximales :** Le transfert des pages HTML précalculées s'effectue sans bascule de contexte (_Context Switch_) inutile entre l'espace utilisateur Rust et l'espace noyau. Le débit s'aligne strictement sur les limites physiques du contrôleur réseau et de l'OS Page Cache.
- **Sécurité Mémoire Native :** L'élimination des manipulations de pointeurs bruts au runtime au sein d'arènes dynamiques supprime tout risque de régression de type _overread_ ou de _panic_ applicatif sur le chemin critique.
- **Souveraineté du Schéma Restaurée :** PostgreSQL redevient l'unique autorité de structuration, de cohérence et de sécurité des données, éliminant les dépendances cycliques entre la compilation des vues et le DDL de la base.s
- **Découplage Temporel Strict :** Le coût CPU du rendu HTML et de l'évaluation des rôles est payé une seule fois lors de la mutation de la donnée, isolant totalement le Read Path des fluctuations de charge du backend.
