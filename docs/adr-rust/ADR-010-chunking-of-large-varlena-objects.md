# ADR-010 : Chunking des champs varlena volumineux — restaurer l'invariant cache L1

Statut : **Proposé** — problème posé, alternatives explorées, décision NON tranchée.
Contexte projet : Marius, pipeline AOT (`marius-db-forge` / `marius-fragment-forge`).
Documents liés : ADR-007 (frontière Hot/Cold), `manifest-reactive-projection.md` §5
(Collector/Dispatch, buffer unique réutilisé), `CONTRAT-implementation-varlena-raw.md`
(TODO posé dans `introspect.rs`, session du 22/07/2026).

---

## 1. Contexte

`content.body.content` a été borné à `VARCHAR(32000)` avec `EscapePolicy::Raw`
pour rester sous le seuil AOT absolu de 64 Ko (`introspect.rs`). Cette borne est
un choix PoC assumé, pas une contrainte produit — un article réel peut légitimement
dépasser 32 000 caractères. Le seuil de 64 Ko lui-même n'est pas arbitraire : il
protège l'invariant posé par `manifest-reactive-projection.md` §5 — un buffer
unique, alloué une fois, réutilisé et vidé entre chaque enregistrement d'un lot,
dimensionné pour saturer le cache L1 (32-48 Ko typique) plutôt que de distribuer
le rendu sur plusieurs cœurs. Un champ varlena de plusieurs centaines de Ko à
plusieurs Mo, même avec `EscapePolicy::Raw` (facteur 1, pas de multiplication),
rend ce modèle intenable pour tout composant qui le porte : `{NAME}_TOTAL_CAP`
grossit avec la donnée, pas avec le schéma, et cesse d'être une borne statique
utile pour le dimensionnement du buffer partagé.

**Ce que ce document ne couvre pas** : le mécanisme `raw` lui-même
(`EscapePolicy`, déjà implémenté) reste valide et nécessaire indépendamment de
ce chunking — les deux sujets sont orthogonaux. Ce document ne remet pas non
plus en cause le seuil de 64 Ko comme garde-fou par défaut ; il explore comment
servir les composants qui doivent légitimement le dépasser.

---

## 2. Le problème n'est pas seulement le stockage

Deux couches sont concernées, et une fragmentation côté PostgreSQL seule
(la piste évoquée en session : « chunks directement dans la table ou via
triggers ») ne résout que la moitié du problème :

1. **Couche stockage** : comment PostgreSQL représente physiquement un contenu
   de taille arbitraire — c'est un problème connu, TOAST le fait déjà de façon
   transparente pour tout varlena au-delà de ~2 Ko. Un chunking applicatif
   explicite (table `content.body_chunk`, ou trigger de fragmentation) n'apporte
   rien à *cette* couche que TOAST ne fasse pas déjà — sauf si l'objectif est de
   permettre une lecture **partielle** (pagination, streaming) sans matérialiser
   tout le champ en mémoire côté PostgreSQL avant transmission, ce que TOAST ne
   permet pas nativement (`SELECT content` désassemble tout le TOAST en un
   `bytea`/`text` complet côté serveur).
2. **Couche rendu AOT** : c'est ici que se situe le vrai problème structurel.
   Même si PostgreSQL stocke le contenu en chunks proprement lisibles un par un,
   `fragment-forge` génère aujourd'hui un unique `buf.push_str(s)` (ou
   `marius_html_escape`) par champ, sur un buffer unique pré-réservé à
   `{NAME}_TOTAL_CAP`. Fragmenter le stockage sans repenser le rendu ne change
   rien à la taille du buffer nécessaire — il faudrait toujours accumuler tous
   les chunks quelque part avant `push_str`, ou alors changer fondamentalement
   comment le rendu écrit vers sa destination finale.

La décision à prendre porte donc sur **la couche rendu** en premier lieu — le
chunking de stockage est une décision secondaire, dépendante de ce que la couche
rendu exige comme granularité de lecture.

---

## 3. Alternatives étudiées

### 3.1 Chunking de stockage seul, buffer de rendu inchangé (écartée en l'état)

Table `content.body_chunk(document_id, chunk_idx, content_chunk VARCHAR(N))`,
lue via plusieurs `lookup` au lieu d'un seul, résultats concaténés dans le
buffer partagé avant `push_str`. **N'apporte rien** : le buffer doit toujours
contenir la totalité du contenu concaténé au moment du `push_str` — la taille
totale nécessaire est identique à aujourd'hui, seule la lecture en amont est
fragmentée. Le seuil de 64 Ko resterait tout aussi bloquant, juste déplacé du
calcul de capacité `introspect.rs` vers un calcul équivalent ailleurs (somme des
chunks). Ne règle pas le problème posé en §2.2.

### 3.2 Rendu en flux (streaming) — écriture directe vers la destination finale

Pour les composants portant un varlena marqué (`marius:raw` volumineux, ou un
nouveau tag dédié type `marius:streamed`), le corps `render()` généré
n'accumule plus ce champ dans le buffer partagé — il écrit directement vers la
destination finale (socket HTTP, fichier `pack.bin` en cours d'écriture) au fur
et à mesure de la lecture des chunks côté PostgreSQL/store, via un petit buffer
de transit réutilisé (taille fixe, L1-friendly, ex. 8-16 Ko), jamais dimensionné
sur la taille totale du contenu.

**Conséquence architecturale majeure, à assumer explicitement si retenue** :
ceci sort du modèle « un seul `buf.reserve()`, un seul `push_str` par champ,
zéro branche » qui est la promesse actuelle de `fragment-forge` (cf.
`article-0.md` §2, §4 : la Forge documente statiquement toute allocation, mais
ce modèle suppose une capacité connue à la compilation). Un champ streamé
introduit une boucle de lecture/écriture à l'exécution — toujours zéro
allocation dynamique (buffer de transit réutilisé), mais plus « une seule
capacité statique calculée pour toute la page ». Deux sous-catégories de
composants apparaîtraient alors dans le générateur : ceux à capacité
entièrement statique (modèle actuel, inchangé), et ceux portant un champ
streamé (modèle à concevoir). C'est un changement de nature du générateur, pas
une simple extension.

### 3.3 Fragmentation trigger côté PostgreSQL, combinée à 3.2

Trigger `BEFORE INSERT OR UPDATE` sur `content.body` qui répartit
automatiquement `content` en lignes de `content.body_chunk` de taille fixe
(ex. 8 Ko/chunk) — transparent pour le code applicatif écrivant
(`content.create_document`/`content.save_revision` continuent d'écrire
`content.body.content` normalement). Combiné à 3.2 côté lecture : le rendu
streamé itère `content.body_chunk ORDER BY chunk_idx` au lieu de lire un unique
`content.body.content`. Cohérent avec la préférence déjà exprimée en session
(fragmentation côté PostgreSQL, transparente, pas une réécriture des points
d'écriture applicatifs) — mais dépend entièrement de 3.2 pour avoir un effet
sur le vrai goulot (le rendu), pas seulement sur le stockage.

### 3.4 Ne rien streamer, sortir ces composants du modèle buffer-unique-par-lot

Alternative plus radicale : les composants à varlena volumineux ne partagent
plus le buffer réutilisé du lot (`manifest-reactive-projection.md` §5) —
chaque enregistrement de ce type reçoit son propre buffer, dimensionné dynamiquement
(réalloué si besoin, allocation exceptionnelle documentée comme telle). Renonce
explicitement à l'invariant L1 pour cette classe de composants, en l'isolant
proprement plutôt qu'en le diluant pour tous les composants (ce que ferait un
simple relèvement du seuil de 64 Ko, déjà écarté précédemment). Plus simple à
implémenter que 3.2/3.3, mais accepte une régression de performance ciblée
plutôt que de la résoudre.

---

## 4. Ce qui manque pour trancher

Cette session n'a jamais vu le code réel du chemin de lecture chaud
(`handlers.rs::deliver`, `pack_html_index.rs`, `batch_renderer.rs`,
`packfile_builder.rs`) ni le format exact de stockage d'un varlena dans
`pack.bin`/`store.bin` (`PackfileReader`/`PackfileBuilder`, TOC + heap, ADR-007).
Décider entre 3.2/3.3/3.4 sans ces fichiers serait spéculer sur la faisabilité
réelle d'un rendu en flux dans ce format de pack — **je m'arrête ici** plutôt
que de recommander une direction sur la seule base du manifeste et des ADR déjà
lus.

## 5. Décision

**Non tranchée.** Ce document pose le problème et écarte 3.1 comme insuffisante
en l'état ; 3.2/3.3 (streaming + fragmentation trigger) et 3.4 (isolation hors
buffer partagé) restent ouvertes. Arbitrage à faire avec vous, une fois les
fichiers du §4 disponibles — ou plus tôt, si vous avez déjà une préférence de
principe (accepter une régression ciblée assumée vs investir dans un vrai
mécanisme de streaming).

---

_Rédigé à la suite du TODO posé dans `introspect.rs` (session du 22/07/2026,
`CONTRAT-implementation-varlena-raw.md`). Ne pas transformer en Contrat
d'Implémentation avant que la Décision (§5) soit prise et les fichiers du §4
confrontés._
