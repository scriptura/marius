# Manifeste de Synergie : Le Paradigme Marius-Forge

## 1. Vision : L'Invariance par la Forge

Marius n'est pas un logiciel que l'on écrit, c'est un système que l'on forge. Le **Core** (`no_std`) est décrété **cible de compilation immuable** : un artéfact passif, optimisé pour le CPU, dont l'humain ne doit pas manipuler les octets directement.

L'intelligence du système ne réside plus dans l'implémentation du moteur, mais dans la **Forge** (l'outillage de métaprogrammation). Le développeur intervient sur l'intention (SQL, DSL, Configuration) ; la Forge garantit la réalité binaire.

---

## 2. L'Attitude no_std : Performance vs Dogme

La synergie avec la Forge permet de lever l'ambiguïté entre l'efficacité brute et les besoins du web (le texte variable).

- **Le Hot Path (Zéro Allocation) :** Pour le transport des données et le cycle de vie du Dispatcher, la Forge génère des structures `#[repr(C)]` pures et contiguës. Ici, l'allocation est proscrite pour saturer les caches L1/L2.

- **La Projection (Allocation Justifiée) :** Le Core adopte une attitude `no_std` qui autorise la crate `alloc`. La Forge génère les buffers de rendu issus de la compilation des templates `.marius` en acceptant la variabilité intrinsèque du HTML (`String`, `Vec`).

- **Déterminisme Statique :** Toute allocation dans le Core est une conséquence expliquée et explicite de la génération. L'outillage de la Forge documente et sature ces points d'allocation, éliminant toute surprise ou comportement imprévisible au runtime.

---

## 3. Les Silos de la Forge (Unités de Génération)

| Forge        | Invariant Garanti  | Cible de Génération |
| ------------ | ------------------ | ------------------- |
| **DB-Forge** | Symétrie Mécanique |

| Structures `#[repr(C)]` miroirs de PostgreSQL.

|
| **Guard-Forge** | Sécurité Statique

| Traits Rust traduisant les politiques RLS et le confinement.

|
| **Fragment-Forge** | Projection Réactive

| Code Rust natif (`push_str` / `write_fmt`) généré depuis `.marius` (Core) + Routes Axum/HTMX (Shell). |
| **Bridge-Forge** | Flux sans Indirection

| Requêtes SQLx optimisées et orchestrateur basé sur Tokio.

|

---

## 4. Doctrine du Flux et de la Réparation

Le développeur ne "répare" jamais le Core directement. Si une faille apparaît dans le binaire ou si une performance s'effondre :

1. **On ajuste l'Intention :** Modification du schéma SQL ou de la configuration de la Forge.

2. **On améliore l'Outil :** Optimisation de l'algorithme de génération de la Forge.

3. **Le Fail-Safe :** Si une règle est trop complexe pour être forgée, l'outil génère un **Contrat d'Implémentation** sous la forme d'un trait vide. C'est le seul cas où l'humain intervient, via un hook contraint, pour combler une lacune temporaire de la Forge.

---

## 5. Intégration de la Stack (Shell vs Core)

La Forge orchestre la séparation stricte entre le **Shell** (`std`) et le **Core** (attitude `no_std`) :

- **Liaison Automatique :** La Forge génère la "colle" infrastructurelle (routes Axum, handlers HTMX, appels SQLx) pour que le Shell puisse nourrir le Core sans que ce dernier n'ait conscience du réseau ou de l'OS.

- **Dépendances Pilotées :** SQLx et le moteur de compilation `.marius` ne sont pas des bibliothèques manipulatrices utilisées manuellement, mais des outils de bas niveau pilotés de manière automatisée par les macros de la Forge.

---

## 6. Conclusion : La Souveraineté de l'Architecte

L'approche Marius-Forge transforme la programmation en une activité de haute précision. En automatisant la rigueur binaire et la gestion de la mémoire variable, le pipeline libère l'architecte pour qu'il se concentre sur l'unique chose qui compte : la structure de la donnée et sa projection vers l'utilisateur.
