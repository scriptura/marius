# Analyse conceptuelle — pourquoi `EmissionPlan` existe

**Contexte :** suite à la v1 de la spécification frontière AOT/Volatile. Délibération sur la nécessité d'une IR distincte entre `ResolvedRange[]` et `IoSlice[]`.

---

## A. Ce que `ResolvedRange[]` garantit — et ce qu'il ne garantit pas

`ResolvedRange = (ptr, len)`, `Copy`, sans lifetime porté par le type. C'est une paire brute.

Un `Vec<ResolvedRange>` nu (ou tout slice) ne garantit **rien de statique** au-delà de « voici des paires pointeur/longueur ». En particulier, il ne garantit pas :

| Propriété | Garantie par `ResolvedRange[]` seul ? |
|---|---|
| Ordre d'émission | Non — l'ordre du `Vec` est une convention d'appel, pas une propriété du type |
| Correspondance 1:1 avec `SegmentDescriptor[]` | Non — rien n'empêche un slice tronqué, dupliqué ou permuté |
| Durée de validité du pointeur | Non — `(ptr, len)` n'a pas de lifetime ; rien n'empêche que la source sous-jacente soit libérée avant l'émission |
| Génération retenue | Non — la structure ne porte plus la `SourceKey` ni la génération dont elle est issue (par construction, §14 de la spec) |
| Backend visé | Non — c'est une donnée par segment, le backend est une propriété par réponse |
| Cohérence globale de la réponse | Non — rien n'atteste que *tous* les segments attendus sont présents |
| Convertibilité vers `IoSlice` | Oui, trivialement — layout identique. Ce n'est pas une garantie ajoutée, c'est gratuit |

Constat : `ResolvedRange[]` est une donnée de segment. Aucune des garanties qui manquent n'est de granularité segment — elles sont toutes de granularité **réponse**. C'est le signal que l'abstraction manquante n'est pas « un meilleur `ResolvedRange` », mais une structure de niveau réponse.

---

## B. Ce qu'`EmissionPlan` ajoute réellement

Quatre candidats retenus, chacun passé au filtre des 4 questions imposées.

### B.1 — Preuve de complétude et de cardinalité

- **Pourquoi `ResolvedRange[]` ne suffit pas :** aucune vérification que `len(ranges) == budget de segments` (ADR-011 §8, fixé AOT par la Forge). Un plan tronqué ou surnuméraire est structurellement indiscernable d'un plan correct.
- **Pourquoi ça appartient à `EmissionPlan` :** c'est le point de construction — un constructeur `EmissionPlan::build(...)` peut vérifier cette égalité **une seule fois par requête**, coût O(1) relativement au budget K (borné, petit — ADR-011 §8). Une fois vérifié, le type devient une preuve portée ; plus personne en aval ne revérifie.
- **AOT / runtime / backend :** le budget est AOT (Forge). La vérification est runtime, à la construction. Le backend n'a rien à vérifier.
- **Normative :** oui — directement lié à l'invariant de budget déjà normatif (ADR-011 §8).

### B.2 — Préservation d'ordre (pas décision d'ordre)

- **Pourquoi `ResolvedRange[]` ne suffit pas :** l'ordre logique est fixé par `SegmentDescriptor[]` (AOT), mais rien ne garantit statiquement que le `Vec<ResolvedRange>` construit au runtime a préservé cette correspondance positionnelle — un bug d'indexation produirait une émission silencieusement incorrecte.
- **Pourquoi ça appartient à `EmissionPlan` :** le constructeur est le seul endroit où l'on dispose encore, simultanément, de `SegmentDescriptor[]` et des `ResolvedRange[]` résolus, pour affirmer la correspondance index-à-index avant que cette information ne soit perdue.
- **AOT / runtime / backend :** l'ordre lui-même est AOT. Sa préservation est vérifiée au runtime, une fois. Le backend consomme un ordre déjà garanti, sans le réinterpréter.
- **Normative :** oui, mais **`EmissionPlan` ne décide pas l'ordre** — il ne fait qu'attester que l'ordre reçu correspond à celui imposé par l'artefact. Ne pas confondre les deux (cf. D).

### B.3 — Rétention de durée de vie (lifetime anchor)

- **Pourquoi `ResolvedRange[]` ne suffit pas :** `(ptr, len)` est sans lifetime. Rien n'empêche que le handle sous-jacent (`Arc<PackHtmlIndex>`, réservation `RequestArena` pour le Volatile) soit libéré entre la résolution et l'émission effective (`writev`/`sendmsg`).
- **Pourquoi ça appartient à `EmissionPlan` :** c'est le seul objet dont la durée de vie couvre exactement la fenêtre « résolu → émis ». Il doit retenir les handles nécessaires (par ex. les `Arc` des générations engagées à l'Étape 1 de la spec §13) pour la durée de sa propre existence.
- **AOT / runtime / backend :** runtime — construit et détruit par requête. Le backend ne voit jamais ces handles, uniquement les pointeurs bruts qu'ils garantissent valides.
- **Normative :** oui — sans cette garantie, fenêtre de use-after-free entre résolution et émission.

### B.4 — Transport de `EmissionBackendKind`

- **Pourquoi `ResolvedRange[]` ne suffit pas :** c'est une donnée par segment ; le choix de backend est une décision par réponse, fixée par la Forge (correction B.2 de la spec).
- **Pourquoi ça appartient à `EmissionPlan` :** `EmissionPlan` est déjà l'objet de granularité réponse ; il porte ce champ jusqu'au backend sans le recalculer.
- **AOT / runtime / backend :** valeur décidée AOT (Forge), simplement transportée au runtime. Ce n'est pas un invariant calculé — c'est un champ obligatoire.
- **Normative :** la présence du champ est normative ; ce n'est pas une garantie vérifiée, c'est une donnée transportée.

### Candidat rejeté — convertibilité directe vers `IoSlice[]`

Ce n'est pas une garantie qu'`EmissionPlan` ajoute : `ResolvedRange` et `IoSlice` ont (ou doivent avoir) un layout identique par construction. La convertibilité est une propriété de la *représentation choisie pour `ResolvedRange`*, gratuite, pas un service rendu par `EmissionPlan`. La retenir comme justification serait le « joli wrapper » explicitement à exclure.

---

## C. `EmissionPlan` est-il minimal ?

Oui, presque exactement dans les termes de l'hypothèse minimale posée en délibération :

```text
ResolvedRange[]  (validé : cardinalité + ordre, une fois, à la construction)
+
EmissionBackendKind  (transporté, jamais recalculé)
+
ancre de durée de vie des générations engagées
```

Aucune garantie candidate supplémentaire n'a résisté à l'examen (B, ci-dessus) sans se réduire soit à une propriété gratuite de `ResolvedRange` (convertibilité), soit à une décision déjà tranchée en amont et qu'`EmissionPlan` ne fait qu'hériter (ordre, génération — cf. D, E).

Ce qui justifie malgré tout un type distinct plutôt qu'un tuple nu `(Vec<ResolvedRange>, EmissionBackendKind)` : le **constructeur**, pas des champs supplémentaires. Un tuple public peut être construit dans n'importe quel état ; un `EmissionPlan` à champs privés et constructeur unique rend l'état invalide *irreprésentable* après construction — la vérification (cardinalité, ordre) est payée une seule fois, à un point unique, puis plus jamais revérifiée sur le chemin chaud. C'est un pattern « validate once, trust the type », cohérent avec l'invariant zéro-allocation/zéro-reconstruction d'ADR-011 §7 : le coût de correction est déplacé à la construction (une fois par requête, O(K)), pas dilué dans le chemin d'émission.

`EmissionPlan` n'est donc pas une IR riche — c'est une capsule de preuve mince. Le nom peut être conservé, mais son contenu doit rester à ces trois éléments ; toute extension future doit repasser par les 4 questions ci-dessus.

---

## D. Ordre d'émission — où il est fixé

```text
SegmentDescriptor[]   → porte l'ordre logique, fixé AOT par la Forge
ResolvedRange[]        → résout les ranges physiques, DOIT préserver cet ordre (responsabilité runtime, non vérifiée par le type seul)
IoSlice[]               → représentation mécanique finale, hérite de l'ordre déjà validé
```

L'ordre n'est jamais décidé par le runtime ni par `EmissionPlan`. Le runtime a la responsabilité de le **préserver** en construisant `ResolvedRange[]` par itération sur `SegmentDescriptor[]` ; `EmissionPlan` a la responsabilité de le **vérifier une fois** avant de devenir une preuve portée. Aucune étape ne déplace la décision d'ordre vers elle-même — seule la vérification se déplace, du néant (aucune vérification aujourd'hui) vers la construction d'`EmissionPlan`.

---

## E. Génération — `EmissionPlan` n'en redéfinit pas la politique

La règle de la spec (§13, Étape 1) ferme déjà la question avant même que `ResolvedRange[]` existe : une seule génération publiée est résolue par `SourceKey`, pour toute la requête. Deux segments référençant le même `SourceKey` obtiennent nécessairement la même génération, par construction de l'Étape 1 — pas par une vérification a posteriori dans `EmissionPlan`.

Conséquence : `EmissionPlan` n'a pas à *revérifier* la cohérence de génération (elle est déjà impossible à violer une fois l'Étape 1 respectée). Sa seule obligation vis-à-vis de la génération est de **retenir les handles** (cf. B.3) pour que cette génération reste valide jusqu'à la fin de l'émission. Ne pas confondre « garantir la cohérence » (déjà fait, en amont) et « garantir la durée de vie » (rôle propre à `EmissionPlan`).

---

## F. Backend — chaîne respectée

```text
Forge → EmissionBackendKind → EmissionPlan → IoSlice[] → Backend
```

`EmissionPlan` transporte `EmissionBackendKind` sans le recalculer (B.4). Le backend ne connaît toujours aucun type Marius (`Projection`, `SegmentDescriptor`, `SourceId`, `SourceKey`, `SourceSpec`, `MaterializedSource`, `VolatileSlot`, `.marius`) — cette frontière n'est pas affectée par l'analyse ci-dessus.

---

## G. Volatile — traversée homogène par `EmissionPlan`

Distinction à maintenir strictement :

- **Capacité AOT** (`VolatileSlot.capacity`) : borne fixée par la Forge, propriété du descripteur statique.
- **Production runtime** : mécanisme de génération du contenu volatil — **non défini par cette analyse, reste ouvert** (aucun producteur ne doit être inventé ici).
- **Longueur effective** : résultat de la production runtime, ≤ capacité — vérifiée avant résolution, pas par `EmissionPlan`.
- **`ResolvedRange`** : une fois la source volatile matérialisée et sa longueur connue, elle produit un `ResolvedRange` structurellement identique à celui d'une source statique.
- **Préparation de l'émission** : à ce stade, `EmissionPlan` ne distingue plus Volatile de statique — les deux sont des `ResolvedRange` homogènes. Le caractère volatile est épuisé avant d'atteindre `EmissionPlan`.

`EmissionPlan` ne porte donc aucune logique spécifique au Volatile ; il hérite d'un flux déjà uniformisé.

---

## Invariants qui justifient réellement l'existence d'`EmissionPlan`

1. Cardinalité vérifiée une fois : `len(ranges) == budget AOT`.
2. Ordre vérifié une fois : correspondance index-à-index avec `SegmentDescriptor[]`.
3. Rétention de durée de vie : les handles de génération engagés restent valides pour toute la fenêtre résolution → émission.
4. Transport de `EmissionBackendKind` sans recalcul.
5. Frontière de confiance : au-delà de la construction, plus aucune revalidation sur le chemin chaud (le type est la preuve).

## Responsabilités qui doivent rester hors d'`EmissionPlan`

- Décider l'ordre des segments (appartient à `SegmentDescriptor[]`/Forge).
- Décider ou redéduire le backend (appartient à la Forge, §15/§16 de la spec).
- Revérifier la cohérence de génération par `SourceKey` (déjà garantie par l'Étape 1, §13).
- Produire ou interpréter le contenu Volatile (mécanisme distinct, hors périmètre).
- Toute connaissance de `Projection`, de route, de `.marius`, ou de sémantique métier.
- Toute logique de conversion `ResolvedRange → IoSlice` au-delà d'une réinterprétation de layout mécanique.

## Questions encore ouvertes (nécessaires uniquement)

1. Représentation de l'ancre de durée de vie (B.3) : un unique lifetime englobant (si toutes les sources partagent une génération / un registre commun) ou une collection de handles hétérogènes (si plusieurs `SourceKey` de générations distinctes coexistent dans une même réponse) ? Dépend de la réponse à la question de cardinalité des générations engagées par requête — non tranchée par cette analyse.
2. La vérification d'ordre/cardinalité (B.1, B.2) doit-elle être un `debug_assert!` (coût nul en release, risque en prod) ou une vérification inconditionnelle (coût O(K), K borné et petit — ADR-011 §8) ? Recommandation : inconditionnelle, le budget K étant déjà conçu pour être petit — mais c'est un arbitrage DESIGN, pas tranché ici.
3. `const K` (tableau de taille fixe, connue par route à la compilation) versus représentation dynamique (`Box<[ResolvedRange]>`) pour `ranges` — question déjà notée ouverte en spec §17, non rouverte ici faute d'élément nouveau.
4. Mécanisme de production du contenu Volatile — toujours totalement ouvert (G), non affecté par cette analyse.

## Proposition éventuelle de structure Rust

Justifiée uniquement par les invariants 1 à 5 ci-dessus — pas au-delà :

```rust
pub struct EmissionPlan {
    // Ordre = ordre AOT des segments (SegmentDescriptor[]), vérifié à la construction.
    // Cardinalité = budget AOT de la route, vérifiée à la construction.
    ranges: Box<[ResolvedRange]>,

    // Fixé par la Forge, jamais recalculé ici.
    backend: EmissionBackendKind,

    // Ancre de durée de vie : garde vivants les handles de génération engagés
    // (Arc<PackHtmlIndex>, réservations RequestArena) pour toute la fenêtre
    // résolution → émission. Représentation exacte : question ouverte n°1.
    _retain: GenerationGuard,
}

impl EmissionPlan {
    /// Seul point de construction. Échoue si cardinalité ou ordre
    /// ne correspondent pas à `route`. Après succès, l'appelant peut
    /// faire confiance au type sans revalidation.
    pub fn build(
        route: &RouteDescriptor,
        resolved: Box<[ResolvedRange]>,
        backend: EmissionBackendKind,
        retain: GenerationGuard,
    ) -> Result<Self, EmissionPlanError> {
        // vérifie resolved.len() == route.segments.len() (invariant 1)
        // vérifie correspondance d'ordre avec route.segments (invariant 2)
        todo!()
    }

    /// Conversion mécanique, sans logique métier (invariant 5).
    pub fn as_io_slices(&self) -> &[IoSlice] {
        todo!()
    }
}
```

`GenerationGuard` et `EmissionPlanError` restent à définir — dépendent de la question ouverte n°1. Aucun autre champ n'est justifié par l'analyse ci-dessus ; toute addition future doit repasser par les 4 questions de la section B.

---

## Réponse en une phrase

`EmissionPlan` existe parce que `ResolvedRange[]` est une donnée *par segment* sans lifetime ni preuve de complétude, alors que l'émission a besoin d'un objet *par réponse* qui atteste une fois, à la construction, que l'ensemble des segments est complet, dans l'ordre AOT, retenu vivant pour la durée de l'émission — au-delà de quoi il ne fait que transporter une décision déjà prise (le backend) sans en rajouter.
