<img src="./assets/default/images/marius.png" width="1408" height="768" style="width: 100%">

# Marius, moteur de Projection Réactive

[![Architecture](https://img.shields.io/badge/Architecture-ECS%2FDOD-E85D04?style=for-the-badge)]()
[![Design](https://img.shields.io/badge/Design-Data--Oriented-2D6A4F?style=for-the-badge)]()
[![Projection](https://img.shields.io/badge/Projection-AOT%20Native-D90429?style=for-the-badge)]()
[![Zero ORM](https://img.shields.io/badge/Transformation-Zero%20ORM-B7094C?style=for-the-badge)]()
[![Zero JSON](https://img.shields.io/badge/Protocole-Zero%20JSON-89023E?style=for-the-badge)]()
[![Hot Path 2](https://img.shields.io/badge/Chemin%20Chaud-Zero%20Allocation-F77F00?style=for-the-badge)]()
[![Mechanical Sympathy](https://img.shields.io/badge/CPU-Mechanical%20Sympathy-D62828?style=for-the-badge)]()
[![Cache L1](https://img.shields.io/badge/Layout-L1%20Cache%20Friendly-FCBF49?style=for-the-badge)]()
[![I/O Zero-Copy](https://img.shields.io/badge/I%2FO-sendfile(2)%20%7C%20pread-118AB2?style=for-the-badge)]()

Marius est une architecture expérimentale qui repense la construction
d'applications web orientées données. Plutôt qu'empiler les couches
traditionnelles (Base de données → ORM → API JSON → Framework front-end
→ Navigateur), Marius s'inspire des moteurs de jeux vidéo pour proposer
un chemin direct et prédictible entre la donnée brute et l'écran.

## Le concept : moins d'intermédiaires, plus de certitudes

Dans une application web classique, le serveur passe son temps à traduire
des données (du SQL vers des objets, des objets vers du JSON) et le
navigateur passe son temps à reconstruire l'interface. C'est ce qu'on
appelle l'indirection.

Marius supprime ces intermédiaires à travers trois partis pris :

1. **La base de données est souveraine.** PostgreSQL n'est pas un simple
   espace de stockage passif — toute règle métier stricte y réside, validée
   structurellement avant même que le serveur ne démarre.

2. **Pas d'ORM.** Le code dialogue directement avec la base de données.
   Les requêtes SQL sont vérifiées à la compilation par SQLx : si une
   requête ne correspond pas au schéma réel, le projet ne compile pas.

3. **Le serveur est un projecteur (AOT).** Au lieu de construire des pages
   à chaque requête, le serveur écoute la base de données. Dès qu'une donnée
   change, il pré-calcule le fragment HTML correspondant. La lecture devient
   un simple transfert de fichier.

## Comment ça fonctionne ?

### Le cycle de vie d'une donnée

```
Mutation SQL  →  pg_notify  →  Collector Rust  →  Projection  →  Artéfact HTML
```

1. **Mutation** : une transaction SQL est validée (`CALL content.publish_document(42)`).
2. **Signal** : PostgreSQL émet un événement `NOTIFY` contenant l'identifiant modifié.
3. **Collecte** : un worker Rust reçoit le signal et l'insère dans une _table de présence_.
   Plusieurs mutations rapides sur le même identifiant n'en produisent qu'une entrée.
4. **Dispatch** : selon un seuil volumétrique (100 entités) ou temporel (500 ms),
   le lot est extrait et distribué sur tous les cœurs CPU disponibles.
5. **Projection** :chaque entité est transformée en HTML par le pipeline AOT natif,
   compilé en code machine à la construction du binaire.
6. **Distribution** : le serveur Axum sert l'artéfact via `sendfile(2)`.
   Latence de lecture : ~100 µs, coût CPU quasi nul.

### Pourquoi cette architecture est stable sous charge

Le Collector agit comme un _tampon de dédoublonnement_. Une mise à jour
massive (10 000 lignes modifiées en une transaction) produit autant de
signaux `NOTIFY`, mais le Collector ne conserve que les identifiants uniques.
Le pipeline de rendu reçoit une liste dédoublonnée, pas une avalanche.

## Stack technique

| Rôle                   | Outil                 | Raison                                                 |
| ---------------------- | --------------------- | ------------------------------------------------------ |
| Socle système          | **Rust**              | Zéro GC, sécurité mémoire à la compilation             |
| Runtime async          | **Tokio**             | I/O non-bloquants, work-stealing multi-thread          |
| Serveur HTTP           | **Axum + Tower**      | Typage statique, middleware composable                 |
| Projection HTML        | **Génération Native** | Code machine natif (`push_str`) (zéro parsing runtime) |
| Protocole client       | **HTMX**              | Échange de fragments HTML, pas de JSON                 |
| Driver base de données | **SQLx**              | Requêtes validées à la compilation                     |
| Source de vérité       | **PostgreSQL**        | Logique métier, triggers, `LISTEN/NOTIFY`              |
| Pipeline assets        | **build.rs**          | CSS/JS minifiés et hashés avant compilation            |

## Structure du projet

```
.
├── artifacts
├── assets
│   └── default
├── build
│   └── default
├── crates
│   ├── assets
│   ├── core
│   │   ├── collector
│   │   ├── projection
│   │   └── schema
│   ├── forge
│   │   ├── bridge-forge
│   │   ├── db-forge
│   │   ├── fragment-forge
│   │   └── guard-forge
│   ├── marius
│   └── shell
│       ├── render
│       └── server
├── db
│   ├── 00_infra
│   ├── 01_meta
│   ├── 02_identity
│   ├── 03_geo
│   ├── 04_org
│   ├── 05_content
│   ├── 06_commerce
│   ├── 07_cross_fk
│   ├── 08_dcl
│   ├── 09_rls
│   ├── 10_meta_seed
│   ├── 11_audit
│   ├── 12_events
│   ├── dml
│   ├── doc
│   ├── migrations
│   ├── tests
│   └── tools
├── logs
└── scripts
```

## Démarrage rapide

### Pré-requis

- Rust stable — [rustup.rs](https://rustup.rs)
- PostgreSQL 18+ (`sudo apt install postgresql`)

### Installation de la base de données

```bash
# Depuis la racine du dépôt
sudo -u postgres psql -f db/master_init.sql
```

### Lancement du serveur

```bash
cargo run
```

Pour un build optimisé :

```bash
cargo build --release
./target/release/marius
```

## Documentation

Les choix d'architecture et leurs justifications se trouvent dans
`doc/adr/`. Chaque ADR documente un problème, les alternatives
considérées et la décision retenue.

## Pourquoi cette architecture ?

Marius a été conçu pour répondre à des problématiques où les architectures
classiques atteignent leurs limites :

- **Déterminisme des performances** : le temps de réponse est plat et
  prévisible, qu'il y ait 10 ou 10 000 utilisateurs connectés.
- **Sobriété énergétique** : l'absence de Garbage Collector et de traitements
  JavaScript massifs divise l'empreinte mémoire du serveur par dix.
- **Garantie de cohérence** : la base de données et le serveur partagent le
  même dépôt. Toute désynchronisation entre le schéma et le code fait
  échouer la compilation. Si ça compile, l'intégration est cohérente.

## Philosophie

Marius est expérimental et assumé comme tel. L'objectif n'est pas de
remplacer les frameworks existants, mais d'explorer ce qui devient
possible quand on aligne la structure de la donnée, la logique métier
et le rendu sur un seul modèle mental cohérent : **la donnée dirige,
le serveur projette, le navigateur affiche**.
