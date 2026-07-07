# Guide runtime — du `render()` compilé à la requête HTTP servie

> Complémentaire de `guide-fragment-forge.md` (compilation `.marius` → `render()`).
> Ce document couvre la couche suivante : comment `render()` est effectivement
> invoqué, sur quel déclencheur, et ce qui invalide un artefact déjà écrit.
> Périmètre disjoint par construction — voir le renvoi en tête du guide `fragment-forge`.
> **Créé le 7 juillet 2026**, à la suite d'une session de débogage complète du
> pipeline `.marius` → HTTP (cause racine : trigger `NOTIFY` jamais appliqué en base).

---

## Schéma global — les deux pipelines et leur unique jonction

Deux graphes de dépendances totalement disjoints. Aucune arête entre eux
n'existe automatiquement — **une seule** les relie, et elle n'est jamais
déclenchée par le graphe du haut :

```
              BUILD TIME (cargo build)                     RUNTIME (processus vivant)
              =======================                      ==========================

        templates/*.marius                                      UPDATE / INSERT / DELETE (SQL)
                │                                                          │
                ▼                                                          ▼
          fragment-forge                                          trigger PostgreSQL
                │                                                          │
                ▼                                                          ▼
        render() généré (source)                                       pg_notify
                │                                                          │
                ▼                                                          ▼
           cargo build                                               PgListener
                │                                                          │
                ▼                                                          ▼
         ┌─────────────┐                                            Collector::flush()
         │   binaire   │                                                  │
         └──────┬──────┘                                                  ▼
                │                                                  Dispatcher::run()
                │                                                          │
                └──────────── point de jonction ─────────────────────────►│
                              (seul lien entre les deux graphes,           ▼
                               jamais emprunté automatiquement)     regenerate_and_swap()
                                                                            │
                                                                            ▼
                                                                     pack HTML (.bin)
                                                                            │
                                                                            ▼
                                                                      HTTP → pread()
```

**Lecture du schéma** : un `render()` neuf, une fois dans le `binaire`, ne
descend le graphe de droite que si quelque chose franchit le point de
jonction — c'est-à-dire uniquement si un événement SQL (réel ou forcé
manuellement, §3) parcourt tout le graphe de droite depuis le début. Remonter
le graphe de gauche seul (recompiler) n'a **aucun effet observable** sur le
graphe de droite. Toute la difficulté de diagnostic de cette session tient
dans cette unique flèche pointillée : elle est facile à supposer implicite,
elle ne l'est pas.

---

## 0. La question à se poser avant toute autre

Face à un HTML qui ne change pas malgré un `cargo build` réussi, la question
n'est **jamais** « le template est-il correct ? » en premier — c'est : **quel
artefact sur disque a été réellement réécrit, et par quel événement ?**
Trois artefacts distincts existent par table, avec trois producteurs
distincts, trois cycles d'invalidation distincts. Confondre l'un pour l'autre
est la cause la plus fréquente d'un « ça ne marche pas » qui prend des heures
à isoler.

---

## 1. Trois artefacts, trois responsabilités — ne jamais les confondre

| Artefact | Producteur | Contenu | Invalidé par |
| --- | --- | --- | --- |
| `render()` (dans le binaire) | `cargo build` → `fragment-forge` | Code Rust généré depuis `.marius` | Modification du `.marius` ou du schéma SQL, **si et seulement si** `cargo:rerun-if-changed` couvre le fichier modifié (§2) |
| `{table}_store.bin` | `marius-dump` (`dumper::dump_table`) | `#[repr(C)]` — lignes brutes DOD, format de transport interne | Ré-exécution manuelle de `marius-dump` |
| `{table}.bin` (le pack HTML) | `regenerate_and_swap` (`Dispatcher` ou dump initial corrigé) | Fragments HTML déjà rendus, indexés par `(offset, len)` — c'est **ce que `handlers.rs` sert par `pread`** | `NOTIFY` Postgres (§3) — **jamais** un `cargo build`, **jamais** un `marius-dump` non corrigé |

Point de vigilance central, vécu en session : **recompiler `render()` ne touche à aucun des deux `.bin`.** Le pack HTML n'est régénéré que si quelque chose appelle explicitement `regenerate_and_swap` avec le nouveau `render()` déjà lié dans le binaire — recompiler seul ne suffit jamais.

**Confusion fréquente, à ne pas reproduire** : `regenerate_and_swap` **n'interroge jamais `{table}_store.bin`** — il exécute `P::fetch_batch(pool, ids)`, une requête PostgreSQL live (`batch_renderer.rs`, en-tête : « distinct du store.bin »). Les deux artefacts `.bin` n'ont **aucune** dépendance de lecture entre eux ; `store.bin` sert exclusivement `marius-dump`/`marius-verify`, jamais le chemin de régénération du pack HTML.

---

## 2. Piège Cargo — `rerun-if-changed` conditionnel

Une directive `cargo:rerun-if-changed={path}` émise **seulement après** un
test d'existence positif (`if path.exists() { println!(...) }`) ne protège
pas contre l'apparition ultérieure de ce fichier. Cargo fige la liste des
chemins surveillés au dernier build **réussi** ; un chemin jamais mentionné
dans cette liste reste invisible à l'incrémentalité, pour toujours, même
après sa création.

Règle à appliquer systématiquement dans tout `build.rs` de ce projet :
émettre `cargo:rerun-if-changed` de façon **inconditionnelle**, avant tout
test d'existence — y compris sur le répertoire parent, dont le mtime change
dès qu'un fichier y apparaît (filet de sécurité pour le cas « fichier pas
encore créé au dernier build »).

Symptôme observable si ce piège est actif : `cargo build` affiche `Finished`
sans ligne `Compiling` pour le crate concerné, alors qu'un fichier source
vient d'être ajouté. Vérification immédiate : `cargo build -vv` doit
afficher, pour chaque template, une ligne `cargo:warning=template=...` —
son absence signale que le build script n'a pas relu ce composant.

---

## 3. Le seul déclencheur de régénération du pack : `NOTIFY` Postgres

```
UPDATE/INSERT/DELETE (table SQL)
        │
        ▼
trigger PL/pgSQL  ──▶  pg_notify(canal, id::text)
        │
        │  (transitoire — perdu si aucun LISTEN actif au moment de l'émission,
        │   PostgreSQL ne rejoue jamais une notification manquée)
        ▼
PgListener (main.rs)  ──▶  Collector::insert(id)
        │                       │
        │                       ▼ (si seuil atteint) notify_one()
        │                       ▼ (sinon) attente du tick périodique (≈500ms)
        ▼
Dispatcher::run()  ──▶  collector.flush()  ──▶  regenerate_and_swap::<P>(ids, …)
        │
        ▼
BatchRenderer::render_batch()  ──▶  écrit {table}.bin  ──▶  swap atomique du LiveRegistry
```

Conséquences directes, à connaître **avant** de chercher ailleurs :

- **Un changement de code seul (`.marius`, `render()`, logique métier) n'entre jamais dans ce graphe.** Rien ne le déclenche. Si vous venez de corriger un template et voulez voir l'effet sans attendre une vraie écriture applicative, forcez un `UPDATE` trivial :
  ```sql
  UPDATE {schema}.{table} SET {pk} = {pk} WHERE {pk} = {valeur};
  -- ou, pour toutes les lignes d'un coup :
  UPDATE {schema}.{table} SET {pk} = {pk};
  ```
  Le trigger `AFTER UPDATE` se déclenche même si aucune colonne ne change de valeur — c'est le seul rôle de cette commande : réémettre un `NOTIFY`.

- **Le serveur doit déjà être démarré et abonné (`[pg_listener] abonné`) avant l'écriture SQL.** Un `NOTIFY` émis avant que le `LISTEN` ne soit actif est perdu — redémarrer le serveur *après* n'y change rien, il ne rattrape jamais un événement passé.

- **Le trigger lui-même doit exister en base.** Un script SQL de trigger (`triggers_notify_dml.sql` ou équivalent) présent dans le dépôt Git **n'est pas exécuté par `cargo build`, ni par aucun binaire Rust** — c'est une dépendance de déploiement externe au code, à appliquer explicitement (`psql -f ...`) sur chaque environnement (dev local, CI, staging, prod). Absence de trigger = silence total, sans erreur, à aucun niveau de la stack Rust. Vérification directe, indépendante du code applicatif :
  ```sql
  SELECT trigger_name FROM information_schema.triggers
  WHERE trigger_name = 'trg_{ma_table}_notify';
  ```
  Zéro ligne → le script n'a jamais été appliqué à cette base. C'est la vérification à faire **avant** toute hypothèse côté Rust.

---

## 4. Résolution de chemin des artefacts — un seul `artifacts/`, par convention de CWD

`packfile_path_for(key)` (et équivalents) résolvent un chemin **relatif au
répertoire courant du processus au lancement**, jamais via
`CARGO_MANIFEST_DIR` ni un chemin absolu. Conséquences :

- Lancer un binaire (`marius`, `marius-dump`, `marius-verify`) depuis un
  autre répertoire que la racine du workspace crée un `artifacts/` local à
  cet endroit, silencieusement — un second exemplaire du même nom de
  fichier, jamais synchronisé avec celui de la racine.
- `cargo test` exécute avec CWD = répertoire du crate testé, pas la racine.
  Des fixtures de test qui écrivent sous `artifacts/` sans isolement créent
  donc `crates/shell/{render,server}/artifacts/` — des résidus de test, à
  ignorer (`.gitignore`) et nettoyer, jamais à confondre avec l'artefact de
  production.

Règle opérationnelle : toujours lancer les binaires (`cargo run --bin ...`)
depuis la racine du workspace. En cas de doute sur l'artefact réellement lu,
`find / -name "{table}*.bin" -exec ls -la {} \;` révèle immédiatement tout
exemplaire parasite par sa localisation et son mtime.

---

## 5. Contrat `dump.rs` — store **et** pack, jamais l'un sans l'autre

`marius-dump` a deux responsabilités disjointes, correspondant aux deux
artefacts distincts du §1 :

1. `dumper::dump_table` — écrit `{table}_store.bin` (donnée brute DOD).
2. `regenerate_and_swap` — écrit `{table}.bin` (pack HTML servi).

Un `dump.rs` qui n'appelle que (1) produit un environnement où le store est
à jour mais où **aucune requête HTTP ne reflète cet état** tant qu'aucun
`NOTIFY` n'a eu lieu par ailleurs. Un dump initial (premier peuplement d'un
environnement, restauration après incident) doit appeler les deux, dans cet
ordre, pour produire un état immédiatement cohérent avec ce que le serveur
sert.

---

## 6. Checklist de diagnostic — « le HTML ne reflète pas mon changement »

Dans l'ordre, chaque étape élimine une classe de cause avant de passer à la
suivante — ne pas sauter d'étape :

1. **`cargo build -vv`** fait-il apparaître `cargo:warning=template=...` pour
   la table concernée, sans message de fallback ? Sinon → §2 (Cargo).
2. **`stat -c '%y %s' artifacts/{table}.bin`** avant et après une écriture
   SQL délibérée (`UPDATE ... SET pk = pk`) : la taille/mtime change-t-elle ?
   Sinon → §3 (chaîne NOTIFY), en commençant par vérifier le trigger en base
   (`information_schema.triggers`), pas le code Rust.
3. **`find / -name "{table}*.bin"`** : un seul exemplaire, au bon endroit ?
   Sinon → §4 (résolution de chemin).
4. **Le serveur a-t-il été démarré, et son `[pg_listener] abonné` affiché,
   avant l'écriture SQL de test ?** Un `NOTIFY` antérieur au `LISTEN` est
   perdu sans recours — refaire le test dans le bon ordre avant de conclure
   à un bug.
5. Seulement si les quatre points précédents sont vérifiés sains : chercher
   une erreur réelle côté `regenerate_and_swap` (le seul point du graphe qui
   logue explicitement un échec, `eprintln!("[dispatcher] ...")`).

---

_Créé le 7 juillet 2026._
