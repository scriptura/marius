# Post-Mortem Architectural : La Frontière Technologique de Marius

## 1. Symptomatologie de la Dérive (Diagnostic)

La conception du moteur Marius a récemment subi une phase d'hyper-ingénierie (spécification v0.2.1), caractérisée par une dérive de la portée du projet (scope creep).

Le point d'origine de cette dérive réside dans la volonté d'isoler la logique de présentation via un DSL propriétaire (`.marius`). L'application stricte du paradigme Ahead-Of-Time (AOT) à ce DSL a entraîné une cascade de complexité accidentelle :

1. **Du template au parseur :** Le besoin d'interdire les opérations d'exécution (JIT) dans la vue a forcé la création d'un parseur strict.
2. **Du parseur aux Opcodes :** Pour garantir la pureté du rendu, le parseur a été spécifié pour produire un jeu d'instructions machine.
3. **Des Opcodes au Routeur Mémoire :** L'exécution de ces instructions nécessitait une arène mémoire partagée avec la source de vérité, poussant l'architecture vers la gestion directe de buffers via POSIX `shm` et l'injection de contexte via des CTE SQL complexes.

**Conclusion du diagnostic :** Bien qu'intellectuellement cohérente et structurellement alignée avec les principes Data-Oriented Design (DOD), cette spécification résolvait des problèmes appartenant à une génération ultérieure du moteur (v2), introduisant des risques systémiques non spécifiés (divergence de layout mémoire, buffer overreads aveugles).

## 2. Restauration des Invariants (Le Recadrage)

La résolution de cette dérive repose sur le rétablissement d'une frontière stricte entre le pipeline de calcul (Write Path) et le pipeline de délivrance (Read Path).

### A. Le Read Path O(1) Absolu : `sendfile(2)`

La frontière de sortie de Marius v1 est fixée sur l'appel système `sendfile(2)`. L'objectif d'un serveur web haute performance n'est pas de calculer une réponse, mais de transférer un état. En déléguant le transfert de l'artéfact précalculé (HTML) directement du cache page du noyau (OS Page Cache) vers le socket TCP, le pipeline Rust garantit une allocation nulle en espace utilisateur.

### B. L'Authentification AOT : La Sécurité par l'Existence

L'erreur conceptuelle majeure consistait à traiter l'authentification comme un filtre d'exécution sur le Read Path. Dans une architecture AOT pure, l'autorisation est résolue au moment de la compilation de la donnée.

- **Résolution :** PostgreSQL évalue les règles (RLS, `auth_bits`) lors de la mutation.
- **Projection :** Si un rôle n'a pas les droits sur une entité, le Dispatcher ne génère pas l'artéfact pour ce périmètre.
- **Délivrance :** Le middleware Axum extrait l'identité (JWT/Session) de la requête entrante uniquement pour construire le chemin de résolution de fichier cible (ex: `/artifacts/role_id/content/1.html`). Le système retourne l'artéfact via `sendfile(2)` ou renvoie un `404 Not Found`. La logique conditionnelle applicative (`if user.is_allowed()`) est éradiquée.

## 3. Élagage Technologique

Pour aligner l'implémentation sur cette frontière, les éléments suivants sont exclus du périmètre v1 :

- **Machine Virtuelle & Arènes Binaires Dynamiques :** Rejetées. L'état projeté final est un fichier sérialisé sur le disque, bénéficiant gratuitement de la gestion mémoire du noyau Linux.
- **Maud :** Supprimée. Devenue une indirection inutile.
- **Parseur AOT Runtime :** Remplacé par un préprocesseur de build.

## 4. Topologie Définitive (Marius v1)

L'architecture est stabilisée sur ce flux inaltérable :

1. **Source :** PostgreSQL centralise l'état, applique le RLS et émet un signal `pg_notify` en cas d'altération validée.
2. **Pré-compilation (`build.rs`) :** Les fichiers `.marius` sont parsés statiquement pour générer du code source Rust brut (appels `push_str` / `write_fmt`), figeant la logique d'assemblage avant la compilation du binaire.
3. **Write Path :** Le `Collector` (lock-free) dédoublonne les événements de mutation. Le `Dispatcher` déclenche les fonctions de rendu (générées à l'étape 2) et écrit les artéfacts HTML mis à jour sur le disque de manière préemptive.
4. **Read Path :** Une requête HTTP entrante est traitée par un middleware léger qui détermine l'empreinte d'identité, cible le fichier pré-calculé correspondant, et commande au noyau son streaming vers la pile réseau via `sendfile(2)`.

Cette révision garantit la viabilité immédiate du projet tout en préservant le débit nominal du moteur (mesuré en dizaines de GB/s sur les micro-benchmarks), confirmant la suprématie d'une ségrégation stricte entre mutation asynchrone et lecture apatride.
