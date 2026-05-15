# Mémorandum Technique : Orientations de Phase 2 pour le Projet Marius

**Objet :** Transition de la "Zéro-Indirection" vers la "Zéro-Copy" (SHM vs Logical Decoding)
**Contexte :** Validation de l'impossibilité physique du casting direct via le protocole `pgwire` (SQLx) et planification de la rupture technologique.

---

## 1. État des Lieux : La Limite Physique de SQLx

L'audit de Phase 1 a acté que le protocole de transport de PostgreSQL (`pgwire`) impose une sérialisation. Les messages de données (DataRow) incluent des métadonnées de transport (longueurs de champs, types) qui interdisent un casting binaire direct vers des structures Rust `#[repr(C)]`.

Marius opère actuellement en **Zéro-Indirection** (suppression des ORM et du JSON), mais subit toujours une **copie mémoire** et un **parsing de transport**.

## 2. Perspective A : Mémoire Partagée (Shared Memory)

L'objectif est d'abolir le concept de message pour passer à un modèle de **Bus Système**.

* **Mécanisme :** Développement d'une extension PostgreSQL native (`pgrx`) gérant un segment de mémoire partagée (`POSIX shm`).
* **Pipeline :** 1. L'extension intercepte la mutation dans PostgreSQL.
2. Elle écrit la donnée brute dans le segment `shm` selon le layout exact dicté par la **DB-Forge**.
3. Le Core Marius accède au segment via `mmap`.
* **Analyse DOD :** * **Latence :** Temps d'accès RAM (nanosecondes).
* **CPU :** Zéro parsing, zéro pile réseau.
* **Contrainte :** Co-location obligatoire (Appliance). C'est le modèle "Moteur de Jeu" appliqué à la base de données.



## 3. Perspective B : Flux Binaire (Logical Decoding)

L'objectif est de transformer le flux de réplication en un tunnel de structures natives.

* **Mécanisme :** Utilisation d'un *Output Plugin* de décodage logique s'abonnant au WAL (Write-Ahead Log).
* **Pipeline :**
1. Le plugin transforme le journal de transactions en un flux d'octets sans overhead de transport.
2. Marius consomme ce flux comme une suite de structures `#[repr(C)]`.


* **Analyse DOD :**
* **Latence :** Microsecondes (I/O Socket).
* **Modularité :** Permet une topologie distribuée (DB et Core séparés).



## 4. Positionnement Stratégique

Le projet Marius s'oriente prioritairement vers la **Perspective A (Shared Memory)** pour les raisons suivantes :

1. **Cohérence Architecturale :** Aligné avec le cadre cognitif ECS/AOT/DOD où le matériel est la limite.
2. **Vision "Appliance" :** Marius n'est pas conçu pour être un service cloud générique, mais une machine de projection haute performance intégrée.
3. **Abstraction par la Forge :** La DB-Forge doit être le pivot d'abstraction. Elle générera soit le code SQLx (Niveau 1), soit les offsets mémoire pour le `mmap` (Niveau 2).

## 5. Points d'Évaluation

1. **Gestion de la concurrence :** Quelle structure de données atomique (type Ring Buffer SPSC) recommander pour le segment SHM afin d'éviter les verrous (locks) entre PostgreSQL (producteur) et Marius (consommateur) ?
2. **Cycle de vie de la Forge :** Comment structurer le `build.rs` pour qu'il puisse générer à la fois les structures `#[repr(C)]` et les métadonnées nécessaires à l'extension `pgrx` ?
3. **Déterminisme :** Comment garantir l'intégrité du segment SHM en cas de crash brutal d'un des deux processus ?
