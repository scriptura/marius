# Stratégies de Scaling Horizontal pour Marius

**Projet :** Marius — Moteur de Projection Réactive
**Objet :** Architecture de déploiement consolidée — Marius v1.4
**Date :** 9 juin 2026
**Version :** 1.4 (intégration du troisième audit GPT : SPOF PostgreSQL, cohérence CDN, budget des goulots, clarification SHR)

---

## Contexte

Ce document est la version consolidée de l'analyse de scalabilité du système Marius. Il intègre :

- L'audit architectural complet des ADR, du manifeste de projection réactive, de la spécification `.marius`, du manifeste `no_std`, et de la spécification SHR.
- L'audit de friction physique (Gemini) : TLS, stockage réseau, saturation PostgreSQL, révocation temporelle.
- Le premier audit externe (GPT) : distinction entre Core scalable et architecture scalable.
- Le deuxième audit externe (GPT) : nature réelle des stratégies B et D, réalité de SHR, statut de l'invariant de permissions.
- Le troisième audit externe (GPT) : SPOF PostgreSQL, cohérence CDN, budget des goulots d'étranglement.

Le constat, affiné sur quatre itérations, est désormais :

> **Le moteur de projection Marius est intrinsèquement parallélisable et ne constitue probablement pas le facteur limitant d'un déploiement à grande échelle. Les limites apparaîtront d'abord dans l'acquisition des mutations (`fetch_batch`), l'I/O PostgreSQL, et la cohérence de distribution des artefacts. La projection HTML est, de tous les maillons de la chaîne, celui qui saturera en dernier.**

---

## Invariants architecturaux

Ces invariants conditionnent directement les stratégies de déploiement.

| Invariant                                                          | Statut            | Impact sur le scaling                                                                                                                                                                                                                                                                                                         |
| ------------------------------------------------------------------ | ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Projection déterministe : `(Record, VarlenOwned) → HTML` sans état | Fondateur         | Rend le Core parallélisable sans contention                                                                                                                                                                                                                                                                                   |
| Core `no_std` : zéro allocation dynamique sur le hot path          | Fondateur         | Profil mémoire prédictible, instances interchangeables                                                                                                                                                                                                                                                                        |
| Entité unique par spécification de page                            | Fondateur         | Tuple plat, pas de jointure au rendu                                                                                                                                                                                                                                                                                          |
| `PAGE_TOTAL_CAP` borne supérieure exacte                           | Fondateur         | Dimensionnement déterministe des buffers                                                                                                                                                                                                                                                                                      |
| Read Path kernel-space ou délégué au CDN                           | Fondateur         | Zéro code applicatif sur le chemin de lecture                                                                                                                                                                                                                                                                                 |
| **Permissions = Rôle − Restrictions de groupe**                    | **Architectural** | **Conditionne la topologie `/artifacts/{role_id}/`. Si des permissions document-based (ACL, partage ad-hoc) sont introduites, l'architecture de routage devra être révisée dans son ensemble.**                                                                                                                               |
| **Cohérence forte à l'origine, eventual sur les edges**            | **Architectural** | **Propriété fondamentale du modèle CDN. Tout utilisateur lisant depuis l'origine voit la version la plus récente. Tout utilisateur lisant depuis un edge CDN peut voir une version périmée pendant la fenêtre de TTL. Acceptable pour un CMS ; ne le serait pas pour un système nécessitant une cohérence forte distribuée.** |

---

## Budget des goulots d'étranglement

L'ordre probable d'apparition des plafonds, du plus précoce au plus tardif, est estimé comme suit. Cet ordre est une hypothèse de travail — il devra être validé par des benchmarks réels.

| Rang  | Composant                        | Nature                                                                      | Stratégies concernées                                                    |
| ----- | -------------------------------- | --------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| **1** | **`fetch_batch`**                | Requêtes SQL d'extraction après notification. Jointures, agrégations, vues. | Toutes (A, B, C, D). Le Dispatcher adaptatif atténue mais n'élimine pas. |
| **2** | **I/O PostgreSQL**               | Écritures, triggers, vues matérialisées, procédures SECURITY DEFINER.       | Toutes. Indépendant du scaling du Writer.                                |
| **3** | **Queue**                        | Si Stratégie D. Débit de publication/consommation, latence.                 | D uniquement.                                                            |
| **4** | **Projection (`render_page()`)** | Rendu HTML. Prouvé rapide (15 µs/page, zéro allocation).                    | C (gaspillage), D (scaling horizontal).                                  |
| **5** | **Nginx**                        | `sendfile(2)`, validation JWT, kTLS (optionnel).                            | Toutes. Très difficile à saturer.                                        |
| **6** | **CDN**                          | Bande passante, purge, propagation.                                         | A, B, D. Le dernier maillon à saturer en pratique.                       |

**Conséquence clé :** les stratégies B, C, et D ciblent principalement les rangs 3 et 4 (queue et projection). Or les plafonds les plus probables sont les rangs 1 et 2 (`fetch_batch` et I/O PostgreSQL). Ce décalage doit être gardé à l'esprit lors du dimensionnement : optimiser la projection ne sert à rien si l'acquisition est saturée.

---

## Propriétés fondamentales

### 1. Le Core est une fonction pure

La transformation `(Record, VarlenOwned) → HTML` est une fonction pure, sans état mutable partagé. Le rendu est _embarrassingly parallel_.

**Limite :** cela garantit que le rendu scale. Cela ne garantit pas que l'acquisition, l'orchestration, ou la distribution scalent.

### 2. Le Read Path est kernel-space ou délégué au CDN

Sur le Writer local, Nginx + `sendfile(2)` transfère le fichier du disque à la socket sans copie utilisateur. kTLS (Kernel TLS) est une optimisation optionnelle. Le vrai facteur de scaling lecture est le **CDN**, qui absorbe la quasi-totalité du trafic et assure le déport géographique.

### 3. Modèle de cohérence : forte à l'origine, eventual sur les edges

- **Origine (Writer) :** cohérence forte. Après un `rename()` atomique, toute lecture locale retourne la version la plus récente.
- **Edges CDN :** cohérence eventual. Un utilisateur peut voir une version périmée pendant la fenêtre de TTL (5 secondes par défaut). Le protocole `data-state="dirty"` protège l'éditeur lui-même (il ignore les mises à jour serveur pendant l'édition), mais un tiers peut voir un état transitoire.

Cette propriété est un choix architectural assumé, acceptable pour un CMS. Un système nécessitant une cohérence forte distribuée (ex : financier) nécessiterait une architecture différente.

### 4. La sécurité est pré-cuite dans l'arborescence et le JWT

Les artefacts sont stockés dans `/artifacts/{role_id}/{entity_id}.html`. Le `role_id` est extrait du JWT par Nginx, sans appel externe. L'existence du fichier conditionne le droit d'accès. Les restrictions de groupe sont absorbées dans la logique de projection PostgreSQL.

### 5. Le stockage est strictement local

Artefacts écrits sur NVMe local. Stockage réseau synchrone (NFS, S3) écarté. Distribution déléguée au CDN en origin-pull.

---

## Stratégies de déploiement

### Stratégie A : Leader Election — Déploiement de production standard

**Nature :** scaling vertical avec haute disponibilité.

**Topologie :**

```
Machine Writer ┌──────────────────────────────────────────────────┐
               │ PostgreSQL ←──LISTEN/NOTIFY──▶ Rust Writer       │
               │                                      │           │
               │                                      ▼           │
               │                              /artifacts/ (NVMe)  │
               │                                      │           │
               │                              Nginx              │──▶ CDN (origin-pull)
               └──────────────────────────────────────────────────┘

CDN Edge ──▶ Utilisateurs (monde entier)
```

**Write Path :**

1. Mutation PostgreSQL → trigger → `LISTEN/NOTIFY`.
2. Rust Writer (élu via `pg_advisory_lock`) : Collector → Dispatcher → `fetch_batch` → `render_page()`.
3. Écriture atomique (`rename`) sur NVMe local.
4. Si révocation : `unlink(2)` local + PURGE CDN (asynchrone).

**Read Path :**

1. CDN : cache hit → Edge ; cache miss → origin-pull → Nginx JWT → `sendfile(2)`.

**Avantages :**

- Une seule connexion LISTEN/NOTIFY.
- Read Path kernel-space sur l'origine.
- CDN pour la distribution mondiale.

**Inconvénients :**

- **Double SPOF d'écriture :** le Writer (rapidement rééligible) et PostgreSQL (non redondable sans réplication). Si PostgreSQL tombe, la réélection du Writer ne sert à rien.
- `fetch_batch` peut devenir le goulot avant `render_page()` (cf. budget des goulots).

**Indications :** déploiement initial recommandé. Rester sur cette stratégie tant que le Writer unique et PostgreSQL suivent le rythme des mutations.

---

### Stratégie B : Partitionnement par Domaine (Bounded Context)

**Nature :** partitionnement organisationnel. L'ajout d'un nœud nécessite la création d'un nouveau domaine métier — ce n'est pas la charge qui déclenche l'ajout.

**Topologie :**

```
Machine Content ┌──────────────────────────────────────────────┐
                │ PostgreSQL ──LISTEN/NOTIFY──▶ Rust Writer     │
                │                              /artifacts/      │──▶ CDN (origin /content/*)
                └──────────────────────────────────────────────┘

Machine Commerce ┌─────────────────────────────────────────────┐
                 │ PostgreSQL ──LISTEN/NOTIFY──▶ Rust Writer    │
                 │                              /artifacts/     │──▶ CDN (origin /commerce/*)
                 └─────────────────────────────────────────────┘
```

**Write Path :** identique à A, par domaine.

**Read Path :** CDN avec routage par URL.

**Avantages :**

- Pas de SPOF global.
- Connexions LISTEN/NOTIFY disjointes.
- Chaque Writer dimensionnable indépendamment.

**Inconvénients :**

- L'ajout d'un nœud nécessite un nouveau domaine métier — pas de scaling automatique.
- Configuration CDN plus complexe.

**Indications :** domaines métier bien séparés justifiant une isolation opérationnelle. Ne pas confondre avec du scaling horizontal élastique.

---

### Stratégie C : Duplication Acceptée — Bootstrap et validation

**Nature :** duplication sans scaling. La capacité d'écriture n'augmente pas avec le nombre de nœuds.

**Topologie :**

```
Machine 1 ┌──────────────────────────────────────────┐
          │ PostgreSQL ──LISTEN/NOTIFY──▶ Rust Writer │
          │                              /artifacts/  │──┐
          │                              Nginx        │  │
          └──────────────────────────────────────────┘  │
                                                        ├──▶ Load Balancer ──▶ Utilisateurs
Machine 2 ┌──────────────────────────────────────────┐  │
          │ PostgreSQL ──LISTEN/NOTIFY──▶ Rust Writer │  │
          │                              /artifacts/  │──┘
          │                              Nginx        │
          └──────────────────────────────────────────┘
```

**Write Path :** identique sur chaque machine. Artefacts générés de manière déterministe.

**Read Path :** Load Balancer en round-robin.

**Avantages :** zéro coordination, haute disponibilité, implémentation triviale.

**Inconvénients :**

- Gaspillage CPU et I/O × N.
- Charge PostgreSQL : N connexions LISTEN/NOTIFY.
- **Recommandation :** limiter à 3 nœuds pour des charges typiques (limite empirique).

**Indications :** démarrage rapide, validation. Migration vers A ou D recommandée pour la production.

---

### Stratégie D : Queue — Scaling horizontal de la projection

**Nature :** scaling horizontal du CPU de projection uniquement. L'acquisition (`fetch_batch`) reste centralisée.

**Topologie :**

```
Machine Leader ┌──────────────────────────────────────────────────┐
               │ PostgreSQL ←──LISTEN/NOTIFY──▶ Rust Leader       │
               │                                      │           │
               │                              fetch_batch         │
               │                                      │           │
               │                              publish mutations   │──▶ Queue (NATS/Redis/Kafka)
               └──────────────────────────────────────────────────┘

Machine Worker 1 ┌──────────────────────────┐
                 │ consume → render_page()   │──▶ /artifacts/ (NVMe local)
                 └──────────────────────────┘

Machine Worker 2 ┌──────────────────────────┐
                 │ consume → render_page()   │──▶ /artifacts/ (NVMe local)
                 └──────────────────────────┘

CDN ──▶ Utilisateurs (origin-pull depuis les Workers)
```

**Write Path :**

1. Leader : `LISTEN/NOTIFY` → `fetch_batch` → publication dans la queue.
2. Workers : consommation → `render_page()` → écriture locale.

**Read Path :** inchangé. CDN en origin-pull.

**Avantages :**

- Découple l'acquisition (I/O-bound) de la projection (CPU-bound).
- Les Workers scalent horizontalement — ajout de nœuds en réponse à la charge de rendu.

**Inconvénients :**

- **L'acquisition (`fetch_batch`) reste centralisée sur le Leader.** Si `fetch_batch` sature, ajouter des Workers ne résout rien (cf. budget des goulots : rang 1).
- Complexité opérationnelle : queue à gérer.
- Latence additionnelle (publication + consommation).

**Indications :** charge de rendu élevée justifiant plusieurs Workers. Si l'acquisition sature avant la projection, il faut sharder PostgreSQL ou migrer vers SHR.

---

## Tableau comparatif

| Critère                 | A (Leader)                   | B (Partitionnement)             | C (Duplication)          | D (Queue)                       |
| ----------------------- | ---------------------------- | ------------------------------- | ------------------------ | ------------------------------- |
| **Nature**              | Scaling vertical + HA        | Partitionnement organisationnel | Duplication sans scaling | Scaling horizontal (projection) |
| **SPOF écriture**       | Double : Writer + PostgreSQL | PostgreSQL par domaine          | PostgreSQL (N fois)      | Leader + PostgreSQL             |
| **Gaspillage CPU**      | Aucun                        | Aucun                           | × N (≤ 3)                | Aucun                           |
| **Charge PG**           | 1 LISTEN + fetch             | 1 LISTEN + fetch/domaine        | N LISTEN + N fetch       | 1 LISTEN + 1 fetch              |
| **Scaling projection**  | Vertical                     | Vertical par domaine            | Aucun                    | Horizontal                      |
| **Scaling acquisition** | Vertical                     | Vertical par domaine            | Aucun                    | Vertical (Leader)               |
| **Ajout de nœuds**      | Non (SPOF)                   | Nouveau domaine requis          | Non (gaspillage)         | Oui (Workers)                   |
| **Complexité ops**      | Moyenne                      | Moyenne                         | Triviale                 | Élevée                          |
| **Lecture**             | CDN                          | CDN                             | Load Balancer            | CDN                             |

---

## Gestion de la révocation et des caches

**Statut :** nécessite une ADR dédiée.

### Niveau 1 : Micro-caching Nginx (frugalité)

- TTL de 5 secondes. Protège le disque. Fenêtre de vulnérabilité acceptable pour les données non critiques.

### Niveau 2 : Révocation active (réactivité)

- `unlink(2)` local immédiat → 404 sur l'origine.
- PURGE HTTP asynchrone vers le CDN avec Surrogate-Key.

### Limites connues (à spécifier)

- Temps de propagation CDN non instantané (propriété architecturale : cohérence eventual sur les edges).
- Échecs de purge possibles (stratégie de retry à définir).
- Purge partielle possible (fenêtre d'inconsistance à documenter).
- Compatibilité Surrogate-Key variable selon le CDN.

---

## Implémentation du routage JWT

Module recommandé : OpenResty (Lua). Extrait `role_id` du JWT, interpole le chemin, `try_files`. kTLS est une optimisation optionnelle (Linux ≥ 4.13), sans impact architectural.

---

## Points de vigilance

### 1. Le goulot principal est `fetch_batch`, pas `render_page()`

`render_page()` est prouvé rapide (15 µs, zéro allocation après le premier appel). La charge réelle est l'extraction SQL des données après notification. Cf. budget des goulots : rang 1.

### 2. PostgreSQL est le SPOF le plus probable

Même avec un Writer rééligible, PostgreSQL reste une ressource unique dont la panne bloque toutes les écritures. La réélection du Writer ne résout rien si PostgreSQL est indisponible. Une réplication PostgreSQL est nécessaire pour éliminer ce SPOF — mais elle introduit une complexité opérationnelle significative.

### 3. La cohérence est forte à l'origine, eventual sur les edges

Propriété architecturale assumée. Les utilisateurs lisant depuis un edge CDN peuvent voir des versions périmées pendant la fenêtre de TTL. Acceptable pour un CMS.

### 4. SHR déplace la charge, ne la supprime pas

La spécification SHR (v2) remplace `LISTEN/NOTIFY` par un segment de mémoire partagée. Trois cas sont possibles :

- **Cas A — IDs uniquement :** le ring buffer contient des `entity_id`. Le Collector doit encore exécuter un `SELECT` → `fetch_batch` persiste. SHR est une optimisation du canal de notification, pas du fetch.
- **Cas B — Données projection-ready :** le ring buffer contient les données complètes prêtes à être rendues. `fetch_batch` disparaît du Collector, mais la charge SQL est déplacée vers le Background Worker PostgreSQL, qui doit exécuter jointures et agrégations avant d'écrire dans le ring buffer. La charge ne disparaît pas — elle change de lieu.
- **Cas C — HTML déjà rendu :** le BGWorker exécute le rendu et écrit le HTML dans le ring buffer. Ce cas est mentionné pour exhaustivité mais n'est pas compatible avec l'architecture actuelle (le rendu est en Rust, pas dans PostgreSQL).

Dans tous les cas, la charge SQL existe. SHR est un déplacement de charge, pas une suppression. Une ADR SHR dédiée devra spécifier le cas retenu et l'impact sur la charge PostgreSQL.

### 5. La Stratégie D ne scale que la projection

La file de Workers scale `render_page()` (rang 4 du budget). L'acquisition (rang 1) reste sur le Leader. Si l'acquisition sature, il faut sharder PostgreSQL ou migrer vers SHR.

### 6. La Stratégie B est du partitionnement, pas du scaling horizontal

L'ajout d'un nœud nécessite un nouveau domaine métier. Terme exact : partitionnement par bounded context.

### 7. L'invariant de permissions est structurel

Le modèle `/artifacts/{role_id}/` suppose `Permissions = Rôle − Restrictions de groupe`. Toute introduction d'ACL par document, de partage ad-hoc, ou d'exceptions utilisateur rendrait ce modèle inopérant.

---

## Conclusion

Le moteur de projection Marius est intrinsèquement parallélisable. Les limites apparaîtront d'abord dans l'acquisition des mutations (`fetch_batch`), l'I/O PostgreSQL, et la cohérence de distribution des artefacts. La projection HTML est, de tous les maillons, celui qui saturera en dernier.

Chemin de déploiement recommandé :

1. **Stratégie C** pour le bootstrap et la validation.
2. **Stratégie A** pour la production standard. Surveiller `fetch_batch` et l'I/O PostgreSQL.
3. **Stratégie D** si la charge de rendu dépasse la capacité d'un Writer unique — mais seulement si `fetch_batch` n'est pas déjà saturé.
4. **Stratégie B** si les domaines métier justifient une isolation opérationnelle.
5. **SHR (v2)** lorsque la charge d'écriture justifie la colocalisation — avec une spécification explicite du contenu du ring buffer et de la localisation de la charge SQL.

Le Core Rust reste inchangé dans tous les cas. Le scaling est un choix de déploiement, pas une modification architecturale — mais ce choix doit être validé par des benchmarks réels, en particulier sur `fetch_batch` et l'I/O PostgreSQL.

---

_Document final — Session d'audit architectural — 9 juin 2026_
