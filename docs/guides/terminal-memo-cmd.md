# Commandes d'installation sur Ubuntu

```shell
# DDL, initialisation de la base de données
# À lancer depuis la racine du dossier 'db'
sudo -u postgres psql -p 5433 -d postgres -f master_init.sql
```

Alternative connexion :
```shell
psql "postgresql://postgres:loki@localhost/postgres" -f db/master_init.sql
```


```shell
# On injecte des données de test dans la base 'marius' déjà créée
sudo -u postgres psql -p 5433 -d marius -f master_schema_dml.pgsql
```

```shell
# Recalibrage des stats PG
# PostgreSQL a besoin de "compter" les lignes physiquement pour mettre à jour ses statistiques.
sudo -u postgres psql -p 5433 -d marius -c "ANALYZE;"
```

```shell
# Audit de sécurité
# Lecture de l'état de santé DOD/ECS
sudo -u postgres psql -p 5433 -d marius -c "SELECT * FROM meta.v_master_health_audit;"
```

```shell
# Si changement des assets
cargo run --release --bin marius-assets -- ./assets/default
```

```shell
# Compiler
cargo run --bin marius-dump

```shell
# Lancer le serveur
cargo run --bin marius
```
