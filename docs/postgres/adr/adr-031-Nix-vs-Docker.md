# ADR-031 — Déterminisme d'Infrastructure : Nix au lieu de Docker

**Statut** : Adopté

## 1. Contexte

Le projet Marius exige un déterminisme strict, du stockage (PostgreSQL) jusqu'à la projection en mémoire (Rust). Pour garantir la reproductibilité des environnements de développement et de production, un système de gestion des dépendances et d'isolation est requis.

L'approche standard de l'industrie repose sur la conteneurisation (Docker/OCI). Il convient d'évaluer si cette approche est compatible avec les invariants de Symétrie Mécanique et de Zero-Indirection définis dans le manifeste du projet, ou s'il faut privilégier une gestion de paquets purement fonctionnelle (Nix).

## 2. Décision

Adoption exclusive de **Nix** (`flake.nix`, `nixpkgs`, et module NixOS) pour la forge, l'isolation et le déploiement.
**Rejet de Docker** et de la conteneurisation classique pour le périmètre du moteur Core.

## 3. Raisonnement (Alignement DOD / AOT)

Le choix de Nix repose sur l'élimination des couches d'indirection logicielle et réseau introduites par la conteneurisation :

### A. Suppression de l'Indirection Réseau et I/O (Symétrie Mécanique)

Docker isole en créant des namespaces kernel et des réseaux virtuels (`bridge`, `veth`, NAT). Pour un système réactif visant la saturation du lien réseau (AOT HTML projection), ce pont virtuel introduit une latence et une consommation CPU inutiles (overhead réseau estimé entre 5 et 15% à très haute charge).

- **Approche Nix :** L'exécutable généré est natif (Bare-Metal). Il se lie dynamiquement à des bibliothèques statiques situées dans `/nix/store/` via des chemins codés en dur au moment de la compilation (RPATH). Le processus communique directement avec le matériel, sans virtualisation de la pile TCP/IP.

### B. Déterminisme AOT vs État Impératif

Un `Dockerfile` classique exécute des commandes impératives (`RUN apt-get install`). L'état final dépend du moment de l'exécution, violant l'invariant de reproductibilité.

- **Approche Nix :** Une dérivation Nix est une fonction pure : $Hash = f(Code\_Source, Compilateur, Dépendances)$. Nix prolonge le paradigme **AOT (Ahead-Of-Time)** à l'infrastructure. Si le hash des dépendances Cargo (`cargoHash`) est identique, le binaire produit sera garanti bit-à-bit, quel que soit l'état de l'OS hôte.

### C. Layout Mémoire et Disque (Zero-Duplication)

L'utilisation d'images Docker empile des couches (OverlayFS), dupliquant l'empreinte disque pour chaque variation mineure de l'environnement, sans optimisation du cache L1/L2 au niveau de l'OS.

- **Approche Nix :** Le `/nix/store/` maintient un graphe de dépendances adressable par contenu. Si deux environnements nécessitent `openssl`, ils pointent vers le même inoeud physique (hardlink). Le système d'exploitation maximise ainsi le partage de la mémoire vive au niveau des bibliothèques partagées.

## 4. Conséquences

- **Positif :** L'empreinte RAM de l'infrastructure d'exécution est réduite aux stricts besoins du binaire Rust (pas de démon `dockerd` ou de surcharge `containerd`).
- **Positif :** L'idempotence est garantie mathématiquement, rendant obsolètes les scripts de validation de l'environnement d'exécution.
- **Négatif (Dette Cognitive) :** L'écosystème fonctionnel paresseux de Nix impose une barrière à l'entrée plus élevée pour les contributeurs externes habitués aux formats impératifs (Docker Compose).
- **Action Requise :** L'environnement de développement complet (compilateur, linter, base de données locale) doit être encapsulé dans un `devShell` au sein de `flake.nix`, activable via la commande `nix develop`.
