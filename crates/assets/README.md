# marius-assets

**marius-assets** est le compilateur d'assets de **Marius**.

Son rôle est de transformer les ressources statiques d'un thème (CSS, JavaScript, SVG, polices, bibliothèques vendored, etc.) en artefacts prêts à être servis par le Shell, tout en produisant un manifeste décrivant ces artefacts.

Le crate s'exécute **uniquement au moment du build**. Aucun traitement n'est effectué pendant les requêtes HTTP.

## Objectifs

Le compilateur poursuit plusieurs objectifs :

- produire des artefacts versionnés par hash de contenu ;
- générer un manifeste unique des assets ;
- garantir la cohérence des références entre les différents fichiers ;
- détecter les erreurs de configuration le plus tôt possible ;
- préparer des ressources directement exploitables par le Shell, sans transformation supplémentaire.

Le runtime ne découvre ni ne reconstruit les assets : il ne fait que consommer le manifeste produit par `marius-assets`.

## Responsabilités

Le crate est responsable de la compilation des ressources statiques :

- génération des bundles CSS ;
- génération des bundles JavaScript ;
- construction du sprite SVG ;
- préparation des polices ;
- préparation des bibliothèques vendored ;
- calcul des hash de contenu ;
- génération du manifeste d'assets.

À terme, le manifeste constitue le contrat unique entre le compilateur d'assets et le Shell.

## Principes d'architecture

`marius-assets` respecte les mêmes principes que le reste du projet :

- compilation Ahead-of-Time (AOT) ;
- absence d'interprétation au runtime ;
- responsabilité unique ;
- séparation stricte entre build et exécution ;
- validation maximale pendant la compilation.

Le crate ne dépend pas du Shell : il produit uniquement des fichiers.

Inversement, le Shell ne connaît pas la manière dont ces fichiers ont été générés.

## Validation

Le compilateur effectue notamment des vérifications de cohérence telles que :

- validation des références entre CSS et polices ;
- validation des identifiants SVG ;
- détection des collisions de symboles JavaScript ;
- validation des références d'assets.

L'objectif est que toute erreur soit détectée au moment du build plutôt qu'au runtime.

## État du projet

Le périmètre fonctionnel est défini, mais plusieurs décisions d'implémentation restent ouvertes, notamment :

- format exact du manifeste d'assets ;
- politique de déclaration des bibliothèques vendored ;
- convention de nommage de certains artefacts ;
- stratégie de résolution des URL CSS ;
- intégration finale avec le Shell.

Ces choix sont documentés dans `marius-assets-ROADMAP.md`.

---

# Compilation

Depuis la racine du workspace :

```bash
cargo build -p marius-assets
```

Compilation optimisée :

```bash
cargo build --release -p marius-assets
```

Vérification uniquement :

```bash
cargo check -p marius-assets
```

Exécution des tests :

```bash
cargo test -p marius-assets
```

Exécution du linter :

```bash
cargo clippy -p marius-assets --all-targets -- -D warnings
```

Compilation des assets d'un thème (ici celui par défaut) :

```bash
cargo run --release --bin marius-assets -- ./assets/default
```

---

# Place dans le workspace

```
crates/
└── assets/
    └── marius-assets
```

`marius-assets` fait partie des outils de build du projet.

Il produit les artefacts statiques consommés par le Shell, sans dépendance structurelle vis-à-vis de celui-ci.

---

_le 3 août 2026_
