# marius-server

Serveur shell de [Marius](https://github.com/scriptura/marius) : frontière
réseau Axum/Hyper/Tokio, point de composition du runtime, et service HTTP des
packs HTML matérialisés.

> **Crate non publié sur crates.io** (`publish = false`). Ce README documente
> le crate tel qu'il est utilisé dans le dépôt et dans le binaire `marius`.

## Rôle

`marius-server` est le **shell réseau** de Marius. Il ne produit aucune donnée
de simulation et n'interprète jamais de format humain : il assemble les
infrastructures de la strate Plateforme et sert des artefacts déjà matérialisés
par le pipeline AOT.

Ses responsabilités :

- **Boucle HTTP** : acceptation des connexions via `hyper-util`
  (`TowerToHyperService`), `Router` Axum comme service, Tokio comme runtime.
- **Composition** : assemble `marius-render` (pipeline HTML), `marius-collector`
  (déduplication des `NOTIFY` PostgreSQL), `marius-schema` (traits de projection),
  `marius-projection` (catalogue T2A).
- **Routage AOT** : `ASSET_ROUTES` est une `phf::Map` générée au build
  (`build.rs`) à partir du `manifest.toml` produits par la Forge — lookup O(1),
  zéro allocation, zéro initialisation au démarrage.
- **Service des packs** : lit les fragments HTML déjà projetés dans
  `{packfile_key}.bin` via `pread()` — jamais de requête SQL, jamais de rendu
  de template sur le chemin HTTP.
- **Binaire `marius`** : point d'entrée du processus (`src/main.rs`).

## Frontière architecturale

Le serveur est un **pur consommateur passif** des artefacts produits en amont.
Il ne connaît ni entités, ni composants, ni ressources, ni systèmes — il ne
connaît que des **routes**, des **artefacts** et des **segments**.

```text
PostgreSQL ──► (Étage 1) store.bin ──► (Étage 2) pack HTML ──► marius-server ──► HTTP
                                                              (pread, aucun rendu)
