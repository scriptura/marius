# Contrat d'augmentation d'une page Marius

> **Statut : Proposed — post-ADR-011** (mis à jour le 6 octobre 2026 pour refléter le pipeline segmenté stabilisé : §10, §12, §16, §23, §27 et §28)
>
> Ce document formalise le rapport entre une page AOT complète, sa contextualisation par la représentation demandée et l'augmentation éventuelle de cette page par des projections possédant un cycle de production ou de mutation indépendant.
>
> Il complète ADR-011 et ne constitue pas une nouvelle primitive du Runtime.

## 1. Objet

Une page Marius est une représentation AOT complète.

L'architecture permet toutefois qu'une page complète soit accompagnée ou enrichie par des projections dont le cycle de production ou de mutation est indépendant de celui de la page elle-même.

Cette possibilité est appelée ici **augmentation**.

L'augmentation ne transforme pas une page en composition dynamique de fragments. Elle conserve le principe fondamental selon lequel Marius produit des représentations AOT complètes et déterministes.

Le présent contrat établit notamment la distinction entre :

* **contextualisation AOT** d'une représentation ;
* **projection indépendante** ;
* **augmentation** ;
* **Segment** comme primitive mémoire/émission du Runtime.

## 2. Principe fondamental : la page reste complète

Une page Marius reste une représentation complète.

L'augmentation ne signifie donc pas :

```text
page = assemblage dynamique de fragments
```

mais :

```text
page complète
    +
projection indépendante éventuelle
```

La page ne dépend pas, pour son existence ou sa cohérence fondamentale, de la présence de la projection augmentante.

Une projection indépendante peut donc :

* être produite séparément ;
* être matérialisée séparément ;
* posséder son propre cycle d'invalidation ;
* être servie séparément ;
* être synchronisée ultérieurement côté navigateur.

La page elle-même reste une représentation AOT complète.

## 3. Deux phénomènes distincts : contextualisation et augmentation

Il est essentiel de ne pas confondre deux formes de variation.

### 3.1 Contextualisation AOT

Une représentation peut dépendre structurellement de son identité.

Exemple :

```text
/articles
```

doit être représentée avec l'onglet `Articles` comme onglet courant.

Cette variation est **intrinsèque à la représentation de la route**.

Elle doit donc être résolue par la Forge.

Ce n'est pas une augmentation.

### 3.2 Augmentation

Une projection constitue une augmentation potentielle lorsqu'elle possède un cycle de production ou de mutation indépendant de celui de la page qu'elle accompagne.

Exemples :

* notifications ;
* panier ;
* état volatile d'une session ;
* information pouvant changer sans que le contenu principal de la page change.

Dans ce cas, la projection peut être factorisée indépendamment de la page.

## 4. La route n'est pas une dimension combinatoire

Le contexte de route doit être traité séparément des états indépendants.

Pour une route donnée, la représentation canonique est unique.

Ainsi :

```text
/            → représentation canonique de /
/articles    → représentation canonique de /articles
/about       → représentation canonique de /about
```

Pour une route donnée, il n'existe pas plusieurs variantes légitimes de la représentation résultant d'un choix arbitraire de contexte de route.

On a donc :

```text
Route → représentation canonique
```

et non :

```text
Route × choix de contexte → représentation
```

La Forge peut par conséquent spécialiser une représentation pour chaque route connue sans créer une explosion combinatoire.

#### Invariant

> **Pour une identité de représentation donnée, le contexte de route produit au plus une représentation canonique AOT.**

Le nombre de représentations résultant du contexte de route est donc borné par le nombre de représentations/routes effectivement déclarées par la Forge.

Il ne constitue pas une dimension combinatoire analogue à l'état utilisateur, au panier, aux notifications ou à une autre donnée volatile.

## 5. Exemple canonique : l'onglet de navigation courant

Considérons :

```html
<nav>
  <a href="/">Accueil</a>
  <a href="/articles">Articles</a>
  <a href="/about">À propos</a>
</nav>
```

Pour `/articles`, la représentation correcte est :

```html
<nav>
  <a href="/">Accueil</a>
  <span class="current">Articles</span>
  <a href="/about">À propos</a>
</nav>
```

ou toute autre représentation définie par les conventions de présentation.

Le compilateur peut également résoudre à cette occasion :

* `.current` ;
* la présence ou l'absence de `href` ;
* `aria-current` ;
* les classes CSS associées ;
* toute autre propriété dont la valeur est une conséquence déterministe de la route.

Il ne s'agit pas d'une fusion runtime.

Il ne s'agit pas d'une projection augmentante.

Il ne s'agit pas d'un état volatile.

La Forge produit directement la représentation contextualisée :

```text
Route /
    → navigation contextualisée pour /

Route /articles
    → navigation contextualisée pour /articles

Route /about
    → navigation contextualisée pour /about
```

## 6. Règle générale de contextualisation

> **Toute propriété de présentation dont la valeur est une fonction déterministe de l'identité de la représentation demandée et qui est connue au build-time doit être résolue par la Forge dans la représentation AOT correspondante.**

Cela inclut notamment :

```text
route → onglet courant
route → aria-current
route → présence/absence de href
route → classe CSS
route → variante structurelle
route → représentation canonique d'un élément de navigation
```

La liste n'est pas limitée à la navigation.

La propriété déterminante est la suivante :

> la variation est nécessairement induite par l'identité de la représentation.

## 7. Variation locale ≠ composition

Le fait qu'un seul élément d'une page varie ne justifie pas la création d'une projection indépendante.

Par exemple :

```text
page /articles
    └── navigation
          └── Articles.current
```

La navigation peut différer légèrement de celle de `/about` sans devenir une projection indépendante.

La variation locale est absorbée dans la représentation AOT de la route.

Le principe est :

> **Une variation déterminée par la représentation reste dans la représentation.**

Il n'existe aucune raison architecturale de transformer chaque variation locale en segment, projection ou mécanisme d'augmentation.

## 8. Critère fondamental d'augmentation

Une variation ne devient candidate à l'augmentation que lorsqu'elle n'est pas simplement une conséquence nécessaire de l'identité de la représentation et qu'elle possède une existence propre.

Le critère principal est :

> **La projection peut-elle évoluer alors que la représentation de la page reste inchangée, parce qu'elle possède son propre cycle de production ou de mutation ?**

Si oui, elle constitue une candidate à la factorisation en projection indépendante.

Exemple :

```text
/articles
    ├── contenu de l'article
    └── notifications
```

Le contenu de l'article peut rester identique alors que les notifications changent.

Les deux cycles ne sont pas nécessairement liés.

## 9. Une page personnalisée n'est pas nécessairement une page augmentée

La personnalisation n'implique pas automatiquement l'augmentation.

Une page peut être entièrement spécialisée AOT si toutes les informations nécessaires à cette spécialisation appartiennent à un ensemble fini de représentations connues lors de la compilation.

L'existence d'une variation n'est donc pas suffisante.

La question est :

> **Cette variation est-elle une conséquence canonique de la représentation, ou possède-t-elle un cycle de vie indépendant ?**

Ainsi :

```text
route → onglet courant
```

est une contextualisation AOT.

Alors que :

```text
session → nombre de notifications
```

est potentiellement une projection indépendante.

## 10. Projection, Artefact et Segment ne doivent pas être confondus

Ces termes appartiennent à des niveaux différents.

#### Projection

Concept de domaine/Forge.

Elle possède une identité, des sources de données, un cycle d'invalidation et des invariants de cohérence.

La Projection est consommée par l'AOT.

#### Artefact

Produit de la Forge ou du pipeline de projection.

Dans l'implémentation actuelle, le packfile constitue un artefact de lecture.

#### Segment

Primitive du Runtime.

Un Segment désigne une plage mémoire contiguë susceptible de participer à une émission.

Le Runtime ne connaît pas sa provenance sémantique.

#### SegmentDescriptor

Description AOT d'un segment :

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SegmentDescriptor {
    pub source: SourceId,
    pub offset: u64,
    pub len: u32,
    pub flags: SegmentFlags,
}
```

#### SourceId

Identité locale à une route permettant de résoudre un `SegmentDescriptor` vers une source matérialisée.

#### SourceKey

Identité globale d'un artefact/source nommé dans le registre.

#### ProducerKey

Identité opaque d'un producteur de contenu volatile, portée par `SourceSpec::VolatileSlot`. Catalogue distinct de `SourceKey` : elle désigne un producteur, pas un artefact.

#### DOM Target

Identité côté navigateur d'une cible de mise à jour.

Le DOM Target n'est pas un Segment.

## 11. Segment ≠ fragment DOM

Un Segment est une primitive mémoire et d'émission.

Il ne doit pas être défini comme :

```text
un morceau de DOM
```

ni comme :

```text
un composant HTML
```

Un Segment peut contenir de l'HTML, du JSON, du XML, du RSS, du texte ou un autre contenu sérialisé.

La correspondance :

```text
Segment → DOM Target
```

est une préoccupation du contrat navigateur, pas du Runtime de segments.

## 12. Contrat serveur

L'augmentation s'appuie sur les primitives existantes du Runtime.

Le chemin serveur est :

```text
RouteDescriptor
      ↓
SegmentDescriptor[]
      ↓
SourceSpec
      ↓
MaterializedSource[]
      ↓
ResolvedRange[]          ← dernier niveau de représentation Marius
      ↓
frontière transport (Bytes → Body → Hyper)
```

`EmissionPlan` n'est pas une étape d'exécution de ce chemin, et `IoSlice[]` est un détail interne du transport, jamais construit ni possédé par Marius (docs/archived/SPECIFICATION-transport-segmente-t2a.md v2, §2 et §9). Le Runtime ne connaît pas la signification métier de la source.

Il ne sait pas si un segment contient :

* une navigation ;
* un article ;
* des notifications ;
* un panier ;
* un autre contenu.

La sémantique a été consommée en amont par la Forge.

## 13. Contextualisation AOT et RouteDescriptor

La contextualisation de route doit être effectuée **avant le Runtime**.

Le `RouteDescriptor` représente une route déjà compilée.

Il ne contient donc pas une instruction telle que :

```text
if current_route == "/articles"
```

à exécuter lors de la requête.

Il décrit directement la représentation AOT correspondant à la route.

Conceptuellement :

```text
route /articles
      ↓
Forge
      ↓
représentation AOT contextualisée
      ↓
RouteDescriptor
      ↓
SegmentDescriptor[]
```

Le Runtime ne connaît donc pas `.current`.

## 14. Une génération AOT par représentation canonique

Le principe peut être formulé plus précisément ainsi :

> **Une identité de représentation détermine une représentation AOT canonique.**

La Forge ne génère pas toutes les combinaisons possibles d'états externes.

Elle génère les représentations effectivement nécessaires.

Pour les propriétés dépendant de la route :

```text
route
  ↓
contextualisation déterministe
  ↓
représentation canonique
```

Il n'existe donc pas de produit cartésien entre :

```text
route × current_tab
```

puisque `current_tab` est une fonction de `route`.

Cette propriété doit être conservée comme invariant architectural.

## 15. Static et volatile : une distinction orthogonale

Le caractère statique ou volatile d'une source ne doit pas être confondu avec la contextualisation de représentation.

Une représentation peut être :

* contextualisée AOT ;
* entièrement statique ;
* issue de données PostgreSQL ;
* ou composée de sources possédant des cycles différents.

La volatilité caractérise le cycle de vie d'une source.

La contextualisation caractérise la détermination d'une représentation.

Ce sont deux dimensions différentes.

## 16. Production d'une source volatile

Le Runtime définit le chemin mémoire permettant de matérialiser une source volatile :

```text
SourceSpec::VolatileSlot { capacity, producer: ProducerKey }
      ↓
producteur (clé opaque)  →  stockage possédé
      ↓
MaterializedSource::Volatile  →  ResolvedRange
```

La capacité est bornée par la Forge. La production, l'ownership, la longueur effective et le traitement du dépassement de capacité sont définis par le contrat séparé `CONTRAT-volatile-v1.md` (propriétés P1–P8). Le premier producteur réel est `nav_profile` : un `<li>` produit côté serveur à partir d'un `VolatileContext` possédé.

Ce contrat d'augmentation n'introduit toujours aucun mécanisme de rendu runtime, de requête SQL ou de composition dynamique : le producteur ne manipule que du contenu déjà borné par la Forge. La provenance du contexte (aujourd'hui un paramètre de requête expérimental, demain une session) est hors du périmètre de ce contrat.

## 17. Cohérence de génération par requête

La résolution des sources statiques respecte l'invariant suivant :

> **Chaque source statique résolue reste cohérente pendant la durée de vie de la requête.**

Une requête qui résout une source en retient la génération (détention de l'`Arc`) ; elle n'observe jamais une génération partiellement remplacée, quelle que soit la rotation survenue ensuite. La résolution est dédupliquée par `SourceKey` distinct : deux segments qui référencent la même source aboutissent à la même génération.

**Limite actuelle — publication non atomique entre sources.** Les artefacts multiples d'une même représentation segmentée (`head` et `tail`, et le monolithique qui les accompagne) ne bénéficient pas encore d'une publication atomique inter-sources. Ils sont écrits ensemble puis publiés par des `store()` successifs : une requête peut théoriquement observer des générations différentes lors d'une rotation (par exemple un `head` de génération N+1 avec un `tail` de génération N).

```text
source A (head) — génération N+1
source B (tail) — génération N        ← possible, brièvement, pendant une rotation
```

Cette limite n'est pas une propriété garantie puis oubliée : l'identité de génération d'un ensemble corrélé de sources (un « bundle » de génération côté registre : `head`, `tail`, éventuelles autres sources corrélées) est un **sujet de conception futur**, qui nécessite un arbitrage dédié. Aucune solution n'est introduite par ce contrat.

La contextualisation AOT ne modifie pas cet invariant.

## 18. Budget et déterminisme

La Forge connaît les représentations qu'elle produit.

Elle peut donc vérifier :

* le nombre de segments ;
* les capacités ;
* les limites d'I/O ;
* les contraintes de représentation ;
* les bornes nécessaires aux sources volatiles.

Le budget de segments doit rester fini et vérifiable au build-time.

Le contexte de route ne constitue pas une exception à cette règle.

Puisque chaque représentation de route est canonique, son budget peut être calculé individuellement.

## 19. Atomicité de la réponse

Une réponse doit rester cohérente avec la représentation qu'elle matérialise.

L'augmentation ne doit pas introduire de reconstruction dynamique de la page.

Une réponse serveur peut être décrite comme :

```text
Route
  ↓
représentation AOT canonique
  ↓
segments ordonnés
  ↓
émission
```

Une éventuelle synchronisation ultérieure d'une projection indépendante côté navigateur est une opération distincte.

Elle ne modifie pas la définition de la représentation initiale.

## 20. Contrat navigateur séparé

Le navigateur peut posséder un mécanisme permettant de mettre à jour une projection indépendante.

Le présent contrat ne choisit pas ce mécanisme.

Il peut s'agir, selon une décision ultérieure :

* de `fetch` ;
* de SSE ;
* de WebSocket ;
* d'un runtime JavaScript minimal ;
* d'un autre protocole ;
* ou d'une autre stratégie.

Aucune de ces technologies ne doit être introduite dans l'ontologie du serveur de segments.

## 21. Progressive enhancement

Une page Marius doit rester fonctionnelle sans JavaScript.

L'augmentation côté navigateur constitue donc une amélioration progressive.

Le serveur doit être capable de fournir une représentation complète sans dépendre du mécanisme de synchronisation client.

La présence d'une projection volatile ne doit pas transformer Marius en moteur de rendu dépendant du navigateur.

## 22. Invalidation serveur et synchronisation navigateur sont distinctes

Deux événements doivent être distingués :

```text
mutation de la source
        ↓
invalidation / régénération serveur
```

et :

```text
nouvel état disponible
        ↓
synchronisation éventuelle du navigateur
```

Le premier relève du pipeline de projection.

Le second relève du contrat navigateur.

Ils ne doivent pas être fusionnés conceptuellement.

## 23. Hyper/Axum hors du contrat d'augmentation

Hyper et Axum appartiennent à la frontière HTTP.

Ils ne doivent pas contaminer :

* l'ontologie Projection ;
* Segment ;
* SegmentDescriptor ;
* SourceId ;
* SourceKey ;
* MaterializedSource.

La propagation effective des octets vers le socket (écritures partielles, backpressure, écriture vectorisée) appartient à Hyper ; les éventuels mécanismes de zero-copy réseau restent hors du contrat Marius (docs/archived/SPECIFICATION-transport-segmente-t2a.md v2, §6).

Le contrat d'augmentation reste indépendant de cette implémentation.

## 24. Ce que l'augmentation interdit

L'augmentation ne doit pas devenir un prétexte pour introduire :

* un renderer runtime ;
* une interprétation de `.marius` au runtime ;
* une composition dynamique de pages ;
* un JOIN SQL effectué par le renderer ;
* une reconstruction de page à chaque requête ;
* une dépendance obligatoire à JavaScript ;
* une connaissance du DOM dans le Runtime ;
* une connaissance de HTMX dans la Forge ;
* une nouvelle abstraction de fragment concurrente à Segment ;
* une explosion combinatoire artificielle des représentations.

En particulier :

> **Une variation déterminée par la route ne doit pas être transformée artificiellement en augmentation afin d'éviter sa spécialisation AOT.**

## 25. Test architectural de discernement

Pour toute nouvelle variation de présentation, appliquer les questions dans cet ordre.

#### Question 1 — La variation est-elle déterminée par l'identité de la représentation ?

Si oui :

→ **contextualisation AOT**.

Exemples :

```text
route → onglet courant
route → aria-current
route → href absent
route → classe CSS
```

La Forge doit la résoudre.

#### Question 2 — Plusieurs états légitimes d'une même représentation peuvent-ils exister indépendamment de cette identité ?

Si non :

→ rester dans la représentation AOT.

Si oui :

→ poursuivre l'analyse.

#### Question 3 — La variation possède-t-elle son propre cycle de production ou de mutation ?

Si non :

→ elle reste une donnée ou une contextualisation de la représentation.

Si oui :

→ **projection indépendante potentielle**.

#### Question 4 — La factorisation apporte-t-elle un bénéfice architectural réel ?

Une projection indépendante ne doit pas être créée simplement parce qu'une portion de HTML peut être isolée.

Elle doit bénéficier d'un cycle de vie indépendant.

## 26. Exemples comparatifs

| Cas                                                        | Classification                                     |
| ---------------------------------------------------------- | -------------------------------------------------- |
| Onglet courant de navigation                               | Contextualisation AOT                              |
| `aria-current` selon la route                              | Contextualisation AOT                              |
| Suppression du `href` de l'onglet courant                  | Contextualisation AOT                              |
| Classe CSS dépendant de la route                           | Contextualisation AOT                              |
| Contenu principal d'une page                               | Projection AOT                                     |
| Panier pouvant changer indépendamment                      | Projection indépendante / augmentation potentielle |
| Notifications                                              | Projection indépendante / augmentation potentielle |
| État de session                                            | Projection indépendante potentielle                |
| Dashboard entièrement déterminé par une représentation AOT | Pas nécessairement une augmentation                |
| Fragment HTML arbitraire                                   | Pas nécessairement une Projection                  |
| Segment mémoire                                            | Primitive Runtime, pas Projection                  |

## 27. Invariants architecturaux

Le contrat impose les invariants suivants.

1. **Une page Marius est une représentation AOT complète.**

2. **Une propriété déterminée par l'identité de la représentation doit être résolue AOT.**

3. **Une route possède une représentation canonique.**

4. **Le contexte de route n'est pas une dimension combinatoire lorsqu'il est une fonction déterministe de la route.**

5. **Une variation locale de présentation ne constitue pas, à elle seule, une augmentation.**

6. **Une projection indépendante est caractérisée par son cycle de production ou de mutation indépendant.**

7. **L'augmentation ne remplace pas la page complète.**

8. **Le Runtime ne connaît pas la sémantique des projections.**

9. **Le Runtime ne connaît pas `.current`, la navigation ou les autres concepts de présentation.**

10. **Le Runtime ne réinterprète pas `.marius`.**

11. **Segment et DOM Target appartiennent à deux niveaux architecturaux distincts.**

12. **ProjectionID, SourceId et SourceKey ne doivent pas être confondus.**

13. **La volatilité caractérise une source et son cycle de vie ; elle ne constitue pas une catégorie de Projection.**

14. **Une source volatile doit avoir une capacité bornée par la Forge.**

15. **La production effective du contenu volatile est définie par un contrat séparé** (`CONTRAT-volatile-v1.md`), jamais par ce contrat d'augmentation.

16. **Chaque source statique résolue reste cohérente pendant la durée de vie de la requête.** La cohérence *entre* sources corrélées d'une même représentation segmentée n'est pas encore garantie (§17).

17. **La réponse HTTP ne doit pas reconstruire dynamiquement la page.**

18. **L'augmentation ne doit pas introduire de dépendance obligatoire à JavaScript.**

19. **La synchronisation navigateur est distincte de l'invalidation serveur.**

20. **Hyper/Axum restent confinés à l'adaptateur HTTP.**

## 28. Périmètre restant à spécifier

Le présent contrat ne clôt pas les sujets suivants :

#### 28.1 Producteurs de `VolatileSlot`

Le contrat de production est défini (`CONTRAT-volatile-v1.md`) et un premier producteur existe (`nav_profile`, alimenté par un contexte de requête expérimental). Restent à spécifier : la source réelle du contexte (authentification, session), un éventuel producteur lisant PostgreSQL, plusieurs producteurs ou plusieurs régions.

#### 28.2 Contrat navigateur

Le mécanisme permettant de cibler et mettre à jour une projection indépendante reste à choisir.

#### 28.3 Transport HTTP

La frontière `ResolvedRange → Bytes → Body → Hyper` est actée (docs/archived/SPECIFICATION-transport-segmente-t2a.md v2). Restent hors périmètre : HTTP/2 et toute optimisation du transport.

#### 28.4 Destin de l'ancien `Projection`

Le trait historique peut encore fusionner des responsabilités qui doivent désormais être distinguées entre Forge, projection et Runtime.

#### 28.5 Zero-copy réseau

`writev`/`sendmsg` et l'absence d'allocation ne constituent pas une garantie de zero-copy réseau.

`MSG_ZEROCOPY` reste un sujet expérimental dépendant de mesures réelles.

## 29. Synthèse

Marius doit être compris comme un compilateur de représentations AOT ordonnées.

Une page est toujours produite comme une représentation complète.

Lorsqu'une propriété de présentation dépend nécessairement de la route, elle est **contextualisée par la Forge**.

Cette contextualisation ne constitue pas une augmentation.

La route détermine une représentation canonique :

```text
Route
  ↓
Contextualisation AOT
  ↓
Représentation canonique
```

Il n'y a donc pas d'explosion combinatoire : pour une représentation donnée, le contexte de route est déterministe et unique.

À l'inverse, lorsqu'une partie de l'état peut évoluer indépendamment alors que la représentation de la page reste inchangée, cette partie peut constituer une projection indépendante.

On obtient alors :

```text
                    représentation AOT
                           │
                           │
              ┌────────────┴────────────┐
              │                         │
       contextualisation          projections
          par la route           indépendantes
              │                         │
              ▼                         ▼
        représentation            augmentation
         canonique                 potentielle
```

Le critère de discernement fondamental est donc :

> **Ce qui est nécessairement déterminé par la représentation appartient à sa contextualisation AOT. Ce qui peut vivre et muter indépendamment de cette représentation peut devenir une projection indépendante.**

Le Segment reste, quant à lui, une primitive de mémoire et d'émission du Runtime ; il n'est ni une projection, ni un fragment DOM.

Ainsi, Marius conserve simultanément :

```text
AOT complet
+
contextualisation canonique
+
factorisation des cycles indépendants
+
Runtime passif
```

sans réintroduire de moteur de composition dynamique.

---

_Document rédigé le 3 septembre 2026_
_Mis à jour le 6 octobre 2026 — §12 aligné sur la SPEC T2A v2 (suppression d'`EmissionPlan`/`IoSlice[]` du chemin), §16 réécrit (production volatile définie par `CONTRAT-volatile-v1.md`), §27.15 et §28 mis à jour, `ProducerKey` ajouté au §10._
