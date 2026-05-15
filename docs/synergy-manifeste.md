# Manifeste de Synergie : Le Paradigme Marius-Forge

## 1. Vision : L'Invariance par la Forge

Marius n'est pas un logiciel que l'on écrit, c'est un système que l'on forge. Le **Core** (`no_std`) est décrété **cible de compilation immuable** : un artefact passif, optimisé pour le CPU, dont l'humain ne doit pas manipuler les octets directement.

L'intelligence du système ne réside plus dans l'implémentation du moteur, mais dans la **Forge** (l'outillage de métaprogrammation). Le développeur intervient sur l'intention (SQL, DSL, Config) ; la Forge garantit la réalité binaire.

## 2. L'Attitude no_std : Performance vs Dogme

La synergie avec la Forge permet de lever l'ambiguïté entre l'efficacité brute et les besoins du web (le texte variable).

- **Le Hot Path (Zéro Allocation) :** Pour le transport des données et le cycle de vie du Dispatcher, la Forge génère des structures `#[repr(C)]` pures et contiguës. Ici, l'allocation est proscrite pour saturer les caches L1/L2.
- **La Projection (Allocation Justifiée) :** Le Core adopte une attitude `no_std` qui autorise la crate `alloc`. La Forge génère les buffers de rendu (Maud) en acceptant la variabilité intrinsèque du HTML (`String`, `Vec`).
- **Déterminisme Statique :** Toute allocation dans le Core est une conséquence explicite de la génération. La Forge documente et sature ces points d'allocation, éliminant toute surprise au runtime.

## 3. Les Silos de la Forge (Unités de Génération)

| Forge              | Invariant Garanti     | Cible de Génération                               |
| ------------------ | --------------------- | ------------------------------------------------- |
| **DB-Forge**       | Symétrie Mécanique    | Structs `#[repr(C)]` miroirs de PostgreSQL.       |
| **Guard-Forge**    | Sécurité Statique     | Traits Rust traduisant les RLS et le confinement. |
| **Fragment-Forge** | Projection Réactive   | Macros Maud (Core) + Routes Axum/HTMX (Shell).    |
| **Bridge-Forge**   | Flux sans Indirection | Requêtes SQLx optimisées et orchestrateur Tokio.  |

## 4. Doctrine du Flux et de la Réparation

Le développeur ne "répare" jamais le Core. Si une faille apparaît dans le binaire ou si une performance s'effondre :

1. **On ajuste l'Intention :** (Schéma SQL ou configuration de la Forge).
2. **On améliore l'Outil :** (L'algorithme de génération de la Forge).
3. **Le Fail-Safe :** Si une règle est trop complexe pour être forgée, l'outil génère un **Contrat d'Implémentation** (un trait vide). C'est le seul cas où l'humain intervient, via un hook contraint, pour combler une lacune temporaire de la Forge.

## 5. Intégration de la Stack (Shell vs Core)

La Forge orchestre la séparation entre le **Shell** (`std`) et le **Core** (`no_std` attitude) :

- **Liaison Automatique :** La Forge génère la "colle" (routes Axum, handlers HTMX, appels SQLx) pour que le Shell puisse nourrir le Core sans que le Core n'ait conscience du réseau ou de l'OS.
- **Dépendances Pilotées :** SQLx et Maud ne sont pas des bibliothèques utilisées manuellement, mais des outils de bas niveau pilotés par les macros de la Forge.

## 6. Conclusion : La Souveraineté de l'Architecte

L'approche Marius-Forge transforme la programmation en une activité de haute précision. En automatisant la rigueur binaire et la gestion de la mémoire variable, nous libérons l'architecte pour qu'il se concentre sur l'unique chose qui compte : la structure de la donnée et sa projection vers l'utilisateur.

---

Ce document est désormais aligné sur l'**Article 0**. Il sanctuarise le fait que le Core peut allouer de la mémoire pour le rendu (HTMX/Maud) tout en restant un espace "interdit" aux modifications manuelles.

Souhaitez-vous que nous passions maintenant à la définition technique d'un de ces silos, ou y a-t-il un dernier ajustement doctrinal à apporter ?
