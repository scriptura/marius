# Spécification — Provisioning de l'espace de projection

Statut : proposition d'architecture, **aucune implémentation**. Aucun numéro de phase assigné ici — à trancher à la discrétion de la suite du roadmap (probablement après la réorganisation de `main.rs` déjà annoncée). Document rédigé en réaction à un échec de démarrage légitime sur environnement vierge, et en prolongement direct du manifeste (`manifest-reactive-projection.md`), pas en rupture avec lui. La primitive d'écriture (§4) et la discrimination absence/corruption (§1, §7) ont toutes deux été confirmées par lecture directe de `regenerate.rs`, `pack_html_format.rs`, `handlers.rs` et `pack_html_index.rs` — aucune hypothèse en suspens, audit complet.

---

## 0. Constat

```
Error: Custom { kind: Other, error: "cold_start: échec ouverture packfile
\"commerce_product_core\" ... No such file or directory (os error 2)" }
```

`LiveRegistry::cold_start` traite aujourd'hui toute erreur d'ouverture de packfile comme un seul cas : fatal. C'est correct pour un fichier corrompu, mais faux pour un fichier qui n'a simplement jamais existé — ce sont deux causes physiquement différentes (`io::ErrorKind::NotFound` contre tout le reste), aujourd'hui confondues dans une seule politique.

Deux résolutions de surface, écartées toutes les deux :

- **Affaiblir `cold_start`** (le rendre tolérant à l'absence) — romprait l'invariant fail-fast pour *tous* les cas, y compris ceux où l'absence est anormale (ex. volume monté mais vide par erreur d'orchestration).
- **Créer des packfiles vides à la main avant `cold_start`** (script, ou code direct dans `main.rs`) — fonctionne, mais oblige le bootstrap à connaître le format binaire (footer, table d'entrées), exactement ce que le manifeste réserve au moteur de projection (§3 : *« Transformateur Pur »*). Ça crée aussi un second site d'écriture du format, dont la dérive avec le premier (`apply_merge_io_sync`) n'est garantie par rien d'autre que la discipline humaine.

Ni l'un ni l'autre n'est acceptable. La question n'est pas *« comment contourner cold_start »*, mais *« quelle responsabilité manque à l'architecture actuelle »*.

---

## 1. Principe directeur — étendre l'invariant fail-fast, pas l'affaiblir

Le manifeste pose déjà la réponse, sans l'expliciter jusqu'au bout (§3, invariant 1) :

> `store.bin` n'est qu'une projection dérivée, jamais une source concurrente — sa fraîcheur dépend du processus d'extraction qui l'alimente, pas d'une autorité propre.

Cet invariant ne concerne pas que `store.bin` (l'instantané d'entrée lu par `fetch_batch`) ; il s'applique tout autant à l'artéfact de sortie — le packfile servi en lecture (`artifacts/{packfile_key}.bin`). Aucun des deux n'a d'autorité propre. PostgreSQL est la seule source de vérité (§3, invariant 1, premier tiret).

Conséquence directe, qui devient le principe de cette spécification :

> **L'absence d'un artéfact de projection n'est pas une incohérence — c'est l'état initial légitime d'un espace de projection qui n'a pas encore été matérialisé. Seule sa présence engage une obligation de validité.**

D'où la classification à trois branches déjà posée en introduction, désormais justifiée plutôt qu'arbitrale :

| État constaté | Signification | Politique |
|---|---|---|
| Absent | Espace de projection jamais matérialisé | Provisionner (créer un artéfact vide et **valide**) |
| Présent, valide | Projection à jour ou simplement ancienne | Charger (`cold_start`, inchangé) |
| Présent, invalide | Violation d'un invariant — l'artéfact n'a pu être écrit que par le seul écrivain légitime (§4), donc une corruption est un événement physique anormal (disque, tronquage, intervention externe), jamais un état de données acceptable | Fatal, immédiat (`cold_start`, inchangé) |

Aucune des deux dernières lignes ne change. Tout l'effort porte sur la première — et sur le fait qu'elle ne doit *rien* coûter aux deux autres.

**Confirmation par audit (`pack_html_index.rs`, §7 point 3)** : ce tableau n'est plus seulement postulé, il correspond exactement au comportement déjà existant de `PackHtmlIndex::open`. L'absence physique d'un fichier produit `io::ErrorKind::NotFound` (via `std::fs::File::open`) ; toute autre anomalie — y compris un fichier présent mais vide ou tronqué, cas frontière qu'on aurait pu craindre mal classé — produit systématiquement `io::Error::other(...)`, donc jamais `NotFound`, donc jamais traité comme une absence. La ligne 3 du tableau (« présent, invalide → fatal ») couvre déjà, sans aucun ajout de code, le cas d'un artéfact partiellement écrit.

**Limite explicite, à ne pas confondre avec ce principe** : « absence d'artéfact » ne veut pas dire « absence de données métier ». Si `artifacts/` est purgé alors que Postgres contient déjà des lignes, provisionner un packfile vide rend le serveur capable de démarrer, mais ne republie pas rétroactivement ces lignes — seules les mutations *futures* (LISTEN/NOTIFY) les feront réapparaître packfile par packfile. La reconstruction complète d'un artéfact à partir de données préexistantes est le rôle déjà confié à `marius-dump` (manifeste §2) ; le provisioning ne s'y substitue pas, et cette spécification ne le couvre pas.

---

## 2. Où vit la responsabilité

Trois candidats, un seul retenu :

- **`main.rs`** : non. C'est la frontière de composition/bootstrap — elle assemble des responsabilités déjà construites ailleurs (`Dispatcher`, `LiveRegistry`, `run_pg_listener`), elle n'en invente jamais une nouvelle qui touche un format binaire. C'est exactement la garde-fou que vous formulez : le bootstrap ne doit jamais savoir ce qu'est un footer.
- **`marius-collector`** : non, par construction — c'est le Core, zéro dépendance, zéro connaissance du système de fichiers ou du concept de packfile (cf. en-tête de `dispatcher.rs` : *« Le Collector reste dans le Core ... Le Dispatcher vit dans le Shell, car il orchestre les I/O »*). Provisionner un fichier est une I/O ; ça ne peut pas être Core par définition déjà actée.
- **`marius-render`** : oui — c'est déjà le seul crate qui connaît à la fois le format (`pack_html_format`), la durabilité (l'écriture atomique tmp + fsync + rename dans `apply_merge_io_sync`) et le cycle de vie de l'artéfact en mémoire (`LiveRegistry`). Le provisioning n'introduit aucune nouvelle frontière de crate — il ajoute une responsabilité supplémentaire à un crate qui porte déjà toutes les connaissances nécessaires, et aucune ailleurs.

`main.rs` ne doit appeler qu'**une fonction nommée pour l'intention** (« assure-toi que cet espace de projection existe »), exposée par `marius-render`, exactement comme il appelle déjà `LiveRegistry::cold_start(ROUTE_TABLE)` sans savoir comment celle-ci ouvre un fichier.

---

## 3. Pourquoi pas un type `ProjectionStorage` / `PackfileStore`

La question est légitime, et la réponse n'est pas qu'une question de goût.

Tout ce que ce système nomme jusqu'ici est concret et non générique au sens objet : `Collector<MAX, WORDS>`, `LiveRegistry`, `BatchRenderer`, `Dispatcher<P, MAX, WORDS>` — des structures monomorphisées ou des fonctions libres (`regenerate_and_swap`, `merge_sweep`, `cold_start`), jamais une interface (`trait`) derrière laquelle plusieurs implémentations pourraient diverger. C'est cohérent avec le manifeste (§5 : *« sympathie mécanique »*, absence d'indirection) et avec votre propre exigence : pas d'abstraction sans bénéfice de polymorphisme réel.

Un `trait ProjectionStorage` n'apporterait aucun bénéfice ici : `ROUTE_TABLE` est connue à la compilation, `'static`, jamais substituée par un autre backend à l'exécution. Introduire une interface pour une seule implémentation, c'est introduire une indirection gratuite — et, plus grave dans ce contexte précis, c'est ouvrir la porte à une *seconde* implémentation future (un mock de test, un backend alternatif) qui pourrait diverger silencieusement du format réel : exactement le risque que cette spécification cherche à éliminer (§4).

**Décision proposée** : pas de type, pas de trait. Une fonction libre, dans `marius-render`, à côté de `apply_merge_io_sync` (même fichier ou module — leur cohabitation documente elles-mêmes qu'elles sont les deux faces du même écrivain). Le nom de la *responsabilité* — utile pour en parler, sans qu'il corresponde à un `struct` — peut rester « **provisioning de l'espace de projection** », ou, si un identifiant de code est utile au site d'appel : `ensure_provisioned` (verbe, comme `cold_start`, pas nom-de-service).

---

## 4. Le mécanisme — un seul écrivain, deux appelants

C'est le cœur de la garantie demandée. La réponse n'est pas une discipline (« n'oubliez pas de garder les deux implémentations synchronisées ») — c'est une contrainte structurelle : **il ne doit pas exister de second site qui écrit un footer.**

**Audit effectué (`regenerate.rs`, `pack_html_format.rs`) — décision actée, plus une hypothèse.**

`apply_merge_io_sync` (`regenerate.rs`) n'est *pas* directement appelable pour le cas vierge : sa signature exige `old: &PackHtmlIndex`, c'est-à-dire un packfile déjà existant et déjà ouvert. Elle mmap l'ancien fichier pour en lire le footer et localiser son index avant de fusionner via `merge_sweep` — il n'y a structurellement rien à fusionner quand il n'y a pas d'ancien fichier. Tenter de la réutiliser telle quelle pour le provisioning, c'est forcer un cas qui ne correspond pas à son contrat d'entrée.

Le format lui-même, en revanche, n'est défini qu'à un seul endroit : `pack_html_format.rs`, explicitement désigné comme tel dans son en-tête (*« Source de vérité unique du format on-disk du packfile HTML »*), important par `batch_renderer.rs` en écriture et `pack_html_index.rs` en lecture. Sa fonction publique `write_packfile_footer(writer: &mut BufWriter<W>, blob_len: u64, index: &[PackfileEntry])` sérialise padding + index + footer pour n'importe quel `Write` — déjà utilisée en production (pas seulement en test) pour matérialiser un packfile à partir de zéro.

`apply_merge_io_sync` ne redéfinit pas le format : elle réutilise les mêmes types `#[repr(C)]` (`PackfileEntry`, `PackfileFooter`), simplement matérialisés directement sur un `mmap` plutôt que via `write_packfile_footer`, parce qu'elle est sur le chemin chaud et ne peut pas se permettre l'indirection d'un `BufWriter`. C'est une seconde *stratégie d'écriture* du même format, pas une seconde définition — la distinction est confirmée par construction (mêmes structs, mêmes constantes `magic`/`version`), pas supposée.

**Conséquence directe** : `write_packfile_footer(writer, 0, &[])` est un cas générique, pas spécial, de la primitive déjà existante — blob vide, index vide, donc `entry_count: 0, index_len: 0`. Le provisioning n'a besoin de rien d'autre. Le corps synchrone est déporté via `spawn_blocking` — pas par prudence locale, mais parce que c'est l'invariant transverse déjà confirmé sur les deux autres sites du système qui touchent un appel système bloquant (write path normal, read path `deliver` — cf. §7, point 2) :

```rust
// Corps synchrone — regenerate.rs ou pack_html_format.rs, voisin direct de
// apply_merge_io_sync. Déléguant la sérialisation du format à
// write_packfile_footer (existante, pub, déjà désignée source de vérité
// unique) ; ne connaît qu'un chemin, pas de PgPool ni de LiveRegistry.
fn ensure_provisioned_sync(packfile_key: &'static str) -> io::Result<ProvisionOutcome> {
    let final_path = packfile_path_for(packfile_key);
    match fs::metadata(&final_path) {
        Ok(_) => Ok(ProvisionOutcome::AlreadyPresent),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let tmp_path = final_path.with_extension("tmp");
            if let Some(parent) = tmp_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new()
                .write(true).create(true).truncate(true)
                .open(&tmp_path)?;
            let mut writer = BufWriter::new(file);
            write_packfile_footer(&mut writer, 0, &[])?; // blob vide, index vide
            writer.flush()?;
            writer.into_inner().map_err(io::Error::other)?.sync_all()?;
            fs::rename(tmp_path, final_path)?;
            Ok(ProvisionOutcome::Provisioned)
        }
        Err(e) => Err(e), // tout le reste reste fatal
    }
}

// Point d'appel async — seule fonction visible depuis main.rs. Même patron
// que regenerate_and_swap déportant apply_merge_io_sync (regenerate.rs,
// lignes 140-144) : spawn_blocking, jamais d'appel synchrone direct sur un
// worker Tokio, indépendamment de la latence attendue de l'I/O elle-même.
pub async fn ensure_provisioned(packfile_key: &'static str) -> io::Result<ProvisionOutcome> {
    tokio::task::spawn_blocking(move || ensure_provisioned_sync(packfile_key))
        .await
        .map_err(io::Error::other)?
}
```

`main.rs` n'appelle que `ensure_provisioned(key)` — jamais `write_packfile_footer` directement, jamais de connaissance de `PackfileEntry`/`PackfileFooter`. La garantie « un seul écrivain » est satisfaite sans micro-refactor : la primitive de sérialisation existait déjà, isolée, publique, au bon endroit ; seule l'orchestration atomique (tmp + rename), absente pour ce cas précis, est nouvelle — et elle n'écrit aucun octet de format elle-même, elle délègue entièrement à `write_packfile_footer`.

**Un fait à signaler, pas à corriger ici** : `apply_merge_io_sync` recalcule l'arithmétique d'alignement 8 octets en ligne (`(bytes_written + 7) & !7`) plutôt que d'appeler `align8()` définie dans `pack_html_format.rs` — cette dernière est `const fn` privée au module, non exposée. Les deux calculs sont identiques par construction (même formule), mais c'est une micro-duplication d'arithmétique, pas de format ; hors périmètre de cette spécification, à signaler séparément si jugé utile.

---

## 5. Séquencement au boot — pourquoi `cold_start` reste inchangé

Point mécanique à trancher explicitement, parce qu'il a une vraie conséquence sur l'ordre :

`LiveRegistry::cold_start(ROUTE_TABLE)` est aujourd'hui **synchrone** (appelé sans `.await` dans `main.rs`) et c'est lui qui *produit* le `Arc<LiveRegistry>` — il n'en consomme aucun en entrée. `regenerate_and_swap` (la voie réactive normale), à l'inverse, est asynchrone et a besoin d'un `Arc<LiveRegistry>` déjà construit pour y appeler `.store()` après écriture.

Si le provisioning appelait `regenerate_and_swap` tel quel, on aurait une dépendance circulaire : provisionner avant `cold_start` exigerait un registre que seul `cold_start` produit. C'est résolu par le découplage du §4 : le provisioning n'appelle **pas** `regenerate_and_swap` (la fonction haut niveau, couplée au registre vivant) — il appelle directement la primitive d'écriture atomique bas niveau, qui ne connaît qu'un chemin de fichier, pas un `LiveRegistry`. Aucune mise à jour de pointeur mémoire n'a de sens à cet instant : rien ne sert encore de lectures, il n'y a pas de pointeur à faire pivoter.

Ordre de démarrage proposé (extension du §8 de la spec Phase 5, qui reste valide pour tout le reste) :

1. `PgPool::connect` (fatal si échec — inchangé).
2. **Provisioning** : pour chaque `packfile_key` de `ROUTE_TABLE`, `ensure_provisioned(key)?` — fatal si échec pour une raison *autre* que l'absence déjà résolue par construction. Ne dépend d'aucune ressource async/Postgres : pourrait même précéder l'étape 1 sans rien casser, mais reste logiquement à cet endroit pour rester adjacent à `cold_start`, dont il est le prérequis direct.
3. `LiveRegistry::cold_start(ROUTE_TABLE)?` — **strictement inchangé**, code et contrat. Au moment où il s'exécute, l'étape 2 garantit que tout `packfile_key` référencé existe déjà sous une forme au moins valide-vide. La distinction « absent vs corrompu » n'a donc plus besoin d'être faite *dans* `cold_start` — elle a déjà été résolue une étape plus tôt, par construction, pas par une branche conditionnelle ajoutée à du code déjà validé en Phase 3/4.
4. Suite inchangée (Dispatchers, `PgListener`, `axum::serve`, supervision `JoinSet` — Phase 5.3, livrée).

C'est la propriété la plus importante de cette proposition : **zéro changement de comportement ou de signature sur `cold_start`**. Le risque de régression sur un invariant déjà testé est nul par construction, pas par discipline de relecture.

---

## 6. Frontières explicites (hors périmètre de cette spécification)

- **Reconstruction de données préexistantes.** Un packfile absent alors que Postgres contient déjà des lignes est provisionné vide, pas reconstruit — cf. limite posée en §1. Backfill complet = `marius-dump`, déjà hors périmètre par le manifeste.
- **`pages_homepage`.** N'a ni trigger, ni `Collector`, ni `Dispatcher` (spec Phase 5 §0 — cascade ADR-008 §5, différée). Conséquence favorable et non recherchée au départ : parce que `ensure_provisioned` n'est paramétré par aucun type `Projection`, il s'applique uniformément aux trois entrées de `ROUTE_TABLE`, `pages_homepage` compris — son packfile peut donc être provisionné vide exactement comme les deux autres, sans attendre que sa cascade de régénération soit implémentée. Ça ne résout pas la cascade elle-même, qui reste différée ; ça évite seulement qu'elle bloque le tout premier démarrage.
- **Concurrence multi-instance.** Le modèle de déploiement actuel est un processus unique (cf. supervision `JoinSet`, Phase 5.3). Le provisioning tel que décrit suppose un seul processus exécutant cette étape ; plusieurs instances démarrant concurremment contre le même répertoire `artifacts/` n'est pas un scénario couvert ici.
- **Fichiers `.tmp` orphelins** issus d'une écriture atomique interrompue (crash entre `write` et `rename`). Hors périmètre : l'absence du fichier final (post-`rename`) reste le seul signal regardé par `cold_start` et par le provisioning ; le nettoyage d'un `.tmp` résiduel est une question d'hygiène disque séparée, non traitée ici.

---

## 7. Points vérifiés (audit complet, plus d'hypothèse en suspens)

Par discipline (cf. spec Phase 5, qui marque explicitement ses propres zones d'incertitude) :

1. **Séparabilité de l'écrivain atomique — résolu par audit direct de `regenerate.rs` et `pack_html_format.rs`.** La sérialisation du format (padding align8, index, footer) est déjà isolée dans `write_packfile_footer`, fonction publique de `pack_html_format.rs`, désignée par son propre en-tête comme source de vérité unique du format on-disk, déjà consommée en production par `batch_renderer.rs`. `apply_merge_io_sync` ne redéfinit pas le format, elle le matérialise différemment (écriture directe sur `mmap`, pour le chemin chaud) — mêmes types `#[repr(C)]`, mêmes constantes `magic`/`version`. Aucun refactor n'est nécessaire : le provisioning appelle `write_packfile_footer(writer, 0, &[])`, cas générique de la fonction existante (blob vide, index vide), pas une branche ajoutée pour l'occasion. Seule l'orchestration atomique (tmp + `fsync` + rename) est nouvelle, et elle n'écrit aucun octet de format — voir §4.
2. **Synchrone ou via `spawn_blocking` — résolu par audit direct de `handlers.rs`.** `deliver()` (Read Path, hot path) déporte `read_at` (un `pread(2)`) dans `spawn_blocking`, avec une justification explicite dans son propre commentaire : *« évite de geler un worker Tokio pendant l'appel système, même backé par le page cache »*. La discipline du projet n'est donc pas conditionnée à un risque de blocage perceptible (latence attendue, contention) — elle est inconditionnelle : tout appel système bloquant en contexte async passe par `spawn_blocking`, point. C'est la troisième confirmation indépendante de cette règle dans le système (write path normal via `regenerate_and_swap`/`apply_merge_io_sync` ; read path via `deliver`), donc un invariant transverse, pas une convention locale à un module. **Conséquence directe pour cette spécification** : mon hypothèse provisoire précédente (« rien ne sert encore au moment du provisioning, donc un appel synchrone direct est probablement sans risque ») est exactement le raisonnement par exception que cette discipline rejette — elle juge l'appel bloquant lui-même, pas son contexte d'exécution. `ensure_provisioned` doit donc déporter son I/O (ouverture, écriture, `flush`, `fsync`, `rename`) dans `tokio::task::spawn_blocking`, au même titre que les deux autres sites, pour rester cohérent avec l'invariant déjà établi — pas par prudence ponctuelle, mais par alignement avec une règle déjà actée ailleurs dans le système.
3. **Sémantique exacte de « présent » — résolu par audit direct de `pack_html_index.rs`.** `PackHtmlIndex::open` distingue déjà, par construction et non par convention, deux familles d'erreurs : l'absence physique du fichier (`std::fs::File::open` échoue avec `io::ErrorKind::NotFound`, le même `ErrorKind` que celui déjà testé par `ensure_provisioned` via `fs::metadata`) contre toute autre anomalie — fichier trop court pour un footer, magic invalide, version inconnue, `index_len` incohérent — systématiquement construite via `io::Error::other(...)`, donc jamais `NotFound`. Un fichier présent mais vide ou tronqué (ex. un `.tmp` renommé prématurément après une écriture interrompue) tombe directement sur la branche « trop court pour contenir un footer » (`file_len.checked_sub(FOOTER_SIZE)` échoue dès `file_len < 32`), testée explicitement (`file_shorter_than_footer_returns_err_never_panics`) — donc sur la branche fatale déjà existante de `cold_start`, sans qu'aucun cas particulier ne soit nécessaire ni dans `ensure_provisioned` ni dans `cold_start`. Les deux sites (`fs::metadata` côté provisioning, `File::open` côté lecture) interrogent la même primitive système pour la même cause physique — aucune logique à synchroniser entre eux, juste deux appels indépendants à un même contrat OS.

---

## 8. Critères de validation (niveau architecture, pas encore de tests)

- Démarrage sur environnement vierge (aucun fichier sous `artifacts/`, base migrée mais sans nécessairement de lignes) : aucune erreur fatale, les trois entrées de `ROUTE_TABLE` ont un packfile valide-vide après le boot, `cold_start` les charge sans branche spéciale.
- Démarrage avec packfiles déjà présents et valides (cas actuel, Phase 5.3) : comportement strictement inchangé, provisioning no-op pour les trois entrées.
- Un packfile présent mais tronqué/corrompu continue de provoquer un arrêt fatal immédiat, sans jamais passer par la branche de provisioning (l'erreur doit être visiblement distincte d'un cas d'absence, dans les diagnostics).
- Aucun appel direct à `pack_html_format` ni à la primitive d'écriture atomique depuis `main.rs` — un seul point d'entrée (`ensure_provisioned` ou équivalent), exposé par `marius-render`.
- Aucune duplication de logique d'écriture de footer constatée dans le diff final : le provisioning doit être une réduction d'appel (zéro entrée) du même chemin de code que la régénération réactive, pas une fonction parallèle.
- **Comportement du Read Path sur un packfile provisionné vide, sans aucune modification de `handlers.rs`** : `lookup(id)` sur un `PackHtmlIndex` à `entry_count: 0` retourne `None` pour tout `id`, quel qu'il soit — branche déjà existante de `serve_route` (`StatusCode::NOT_FOUND`). Aucune route ne doit produire 500 sur un environnement vierge provisionné : chaque requête sur une entité pas encore projetée doit recevoir un 404 ordinaire, indiscernable d'un id inexistant en base. C'est une conséquence gratuite de la conformité au format (§4), pas une garantie qui nécessite un nouveau test du Read Path — à vérifier une fois, pas à concevoir.
