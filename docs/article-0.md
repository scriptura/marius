# Article Zéro : L'Invariance par Génération et le Paradigme de la Forge

**Préambule**
Le système Marius rejette la conception logicielle artisanale au profit d'une ingénierie de système industriel. Ce document définit la loi constitutionnelle du projet : la séparation absolue entre l'intention humaine et l'exécution machine.

## 1. L'Immuabilité du Core (Artefact Passif)

Le Core (`no_std`) de Marius n'est pas un espace de développement, c'est une cible de compilation.

- Toute modification manuelle du Core est considérée comme une dette technique ou un échec temporaire de l'outillage.
- Bien qu'assimilable à un "bytecode", le code Rust généré doit impérativement rester lisible et documenté par la Forge pour permettre l'auditabilité et le débuggage.

## 2. Le Pragmatisme `no_std` et la Transparence d'Allocation

L'attitude `no_std` n'est pas une quête de pureté absolue interdisant toute allocation dynamique, mais un rejet strict des dépendances au système d'exploitation et des coûts cachés.

- **Hot Path (Zéro Allocation) :** Les cycles critiques (Collector, Dispatcher) s'exécutent avec une stricte interdiction d'allocation dynamique. Ils manipulent des structures `#[repr(C)]` pures.
- **Projection (Allocation Justifiée) :** La crate `alloc` est une dépendance légitime du Core. L'utilisation de types variables (`String`, `Vec`) est autorisée pour la génération des fragments HTML finaux.
- La Forge est l'entité responsable de documenter statiquement toute allocation dans le code généré, éliminant ainsi toute allocation imprévisible.

## 3. La Forge comme Seule Interface d'Intention

L'intention du système (règles métier, topologie des données, projection visuelle) est versionnée exclusivement via le DDL PostgreSQL, les configurations déclaratives et les DSL.

- La Forge lit ces intentions et produit le code d'exécution.
- Si le layout SQL change, la Forge répercute mécaniquement ce changement jusqu'au buffer Rust, garantissant la Symétrie Mécanique.

## 4. La Transparence de l'Échec (Garde-fou)

La Forge ne doit jamais masquer son incapacité à modéliser un problème complexe.

- Si un cas d'usage sort du périmètre des générateurs existants, la Forge ne doit pas produire une solution sous-optimale ou une "boîte noire".
- Elle doit interrompre la génération totale et produire un **Contrat d'Implémentation** (ex: générer un `trait` Rust non implémenté pointant vers la règle métier en échec). Le problème est ainsi circonscrit via des hooks contraints.

## 5. La Topologie des Intervenants

Le paradigme de la Forge redéfinit les profils de l'équipe d'ingénierie :

1. **L'Utilisateur de la Forge :** Modélise la donnée et les vues (SQL/DSL). Il manipule la sémantique sans se soucier du layout mémoire.
2. **L'Architecte de Forge :** Conçoit et maintient les générateurs (DB-Forge, Fragment-Forge). Il garantit que les abstractions de l'utilisateur sont traduites en code AOT optimal.
3. **Le Forgeur du Core (Exception) :** Intervient manuellement sur le Core Rust uniquement via les hooks contraints générés lors d'une anomalie de la Forge. Cette action est toujours classifiée comme temporaire en attendant une mise à jour de l'Architecte de Forge.
