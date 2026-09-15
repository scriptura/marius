# Handoff — Introduction d’un contexte AOT de représentation de route dans Marius

## 0. Mission de cette session

Cette session Claude est dédiée **exclusivement à une évolution du compilateur Forge / `.marius`**.

Objectif unique :

> permettre à un template `.marius` d'accéder à un contexte AOT de représentation de route, afin de produire une représentation canonique différente selon la route compilée, sans introduire la moindre condition ou interprétation au runtime.

Le premier consommateur concret sera le menu de navigation principal.

**Cette session ne doit pas implémenter T2A.**

T2A sera traité ultérieurement dans une autre session, à partir du résultat final validé ici.

---

# 1. Contexte architectural

Marius est un système de projection AOT :

```text
PostgreSQL
    ↓
Projection
    ↓
Forge
    ↓
artefact AOT
    ↓
HTTP
```

`.marius` utilise volontairement une syntaxe inspirée de Jinja.

Le projet réutilise cette syntaxe pour bénéficier des conventions existantes des IDEs et linters, mais **`.marius` n'est pas interprété comme un template au runtime**.

Tout doit être résolu/abaissé/généré à la compilation AOT.

Le système sait déjà compiler des conditions telles que :

```jinja
<article class="content-core" id="{{ record.document_id }}">
  <h1>{{ record.headline }}</h1>
  <h2>{{ record.alternative_headline }}</h2>
  {% if record.is_readable %}
  <p class="body">{{ record.description }}</p>
  {{ record.description }}
  {% endif %}
</article>
```

Le nouveau besoin est de permettre une condition provenant du **contexte de représentation de route**, par exemple :

```jinja
{% if route.current %}
    ...
{% else %}
    ...
{% endif %}
```

La valeur doit être connue au moment de la compilation/matérialisation AOT.

---

# 2. Cas concret : navigation

Le fichier réel concerné est :

```text
crates/core/schema/templates/navigation.marius
```

Il contient actuellement le menu principal, le bouton de menu, les entrées de navigation et le breadcrumb.

Nous souhaitons pouvoir exprimer conceptuellement une entrée comme :

```jinja
<li>
  {% if route.current %}
    <a href="/content/1">
      <svg class="icon-inline" aria-hidden="true">
        <use href="{% asset sprites/utils.svg %}#ampersand"></use>
      </svg>
      Content 1
    </a>
  {% else %}  
    <div class="current">
      <svg class="icon-inline" aria-hidden="true">
        <use href="/sprites/util.svg#ampersand"></use>
      </svg>
      Sample forum
    </div>
  {% endif %}
</li>
```

Les détails HTML ci-dessus représentent **le résultat recherché** ; ne pas tergiverser sur le markup.

Le besoin architectural est :

```text
Route canonique
      ↓
contexte AOT de représentation
      ↓
Forge
      ↓
HTML entièrement résolu
```

et jamais :

```text
HTTP request
      ↓
runtime if route.current
      ↓
HTML
```

---

# 3. Point fondamental : pas d’explosion combinatoire de route

La contextualisation de route ne doit pas être considérée comme une dimension combinatoire de représentation.

Pour une route canonique donnée :

```text
route
  ↓
une représentation finale AOT
```

Exemple :

```text
/content/1
    ↓
Content est l'onglet courant
    ↓
une seule représentation finale
```

Il n'existe pas de génération de :

```text
Content current
Content non-current
Forum current
Forum non-current
...
```

pour une même route.

Une route sans onglet courant produit simplement une représentation où aucun onglet n'est marqué courant.

De plus, plusieurs routes peuvent partager une même représentation canonique de navigation si leur contexte de navigation est identique.

La session doit donc **ne pas inventer une multiplication de variantes URL × état**.

---

# 4. `route.current` n'est pas du Volatile

C'est un point architectural impératif.

`route.current` :

- n'est pas une donnée de session ;
- n'est pas une donnée utilisateur ;
- n'est pas une donnée runtime ;
- n'est pas une projection volatile ;
- n'est pas une augmentation browser ;
- n'est pas du HTMX.

C'est une information déterminée lors de la production d'une représentation AOT.

Ainsi, conceptuellement :

```text
route.current = Some(Content)
```

ou :

```text
route.current = None
```

est résolu avant que l'artefact soit servi.

Le runtime HTTP ne doit jamais connaître cette information.

---

# 5. T2A est hors scope et ne constitue PAS une dépendance

La future chaîne T2A sera :

```text
artefact AOT
    ↓
SegmentDescriptor
    ↓
ResolvedRange[]
    ↓
Bytes
    ↓
Body
    ↓
Hyper
```

La session actuelle ne doit implémenter aucune de ces étapes.

La Forge doit pouvoir compiler et matérialiser une représentation utilisant `route.current` **avant que T2A soit implémenté**.

Il ne doit donc exister aucune dépendance :

```text
Forge → T2A
```

ni :

```text
Forge → SegmentDescriptor
Forge → SourceKey
Forge → ResolvedRange
Forge → Hyper
```

Le futur T2A consommera simplement l'artefact produit.

---

# 6. Syntaxe `.marius`

Le projet utilise une syntaxe inspirée de Jinja et non un langage propriétaire.

Le lexer central est :

```text
crates/forge/fragment-forge/src/fragment/lexer.rs
```

Le système supporte déjà notamment :

```jinja
{{ ... }}
{% if ... %}
{% else %}
{% endif %}
{% import ... %}
{% extends ... %}
{% asset ... %}
```

Ne crée pas un nouveau langage de template.

Ne crée pas une syntaxe propriétaire pour les routes si le mécanisme peut être exprimé proprement dans le modèle existant.

La forme conceptuelle recherchée est :

```jinja
{% if route.current %}
```

La syntaxe exacte du contexte, son type et sa propagation restent à déterminer après inspection du compilateur.

---

# 7. Corpus initial à inspecter

Le dépôt pertinent pour l'investigation est notamment :

```text
crates/forge/fragment-forge/
├── Cargo.toml
├── README.md
└── src
    ├── fragment
    │   ├── codegen.rs
    │   ├── lexer.rs
    │   ├── mod.rs
    │   ├── parser.rs
    │   ├── resolver.rs
    │   ├── script_hoisting.rs
    │   ├── static_markers.rs
    │   ├── token.rs
    │   └── validator.rs
    ├── lib.rs
    ├── naming.rs
    ├── page
    │   ├── blocks.rs
    │   ├── importer.rs
    │   ├── linker.rs
    │   ├── lowering.rs
    │   ├── model.rs
    │   ├── mod.rs
    │   ├── parser.rs
    │   └── token.rs
    └── schema.rs
```

Template concret :

```text
crates/core/schema/templates/navigation.marius
```

Le guide `.marius` du projet, fourni séparément par l'utilisateur, fait également partie du corpus normatif pour comprendre la syntaxe et les concepts existants.

---

# 8. `shell/render` : ne pas l'intégrer au scope par défaut

Le dépôt contient notamment :

```text
crates/shell/render/src/
├── batch_renderer.rs
├── dispatcher.rs
├── dumper.rs
├── emission.rs
├── ingest_and_swap.rs
├── merge_store.rs
├── packfile_builder.rs
├── pack_html_format.rs
├── pack_html_index.rs
├── regenerate.rs
├── registry.rs
├── store_provisioning.rs
└── sweep.rs
```

Ces fichiers ne sont **pas automatiquement dans le scope** de cette session.

Si l'investigation démontre qu'un point précis de `render` construit, transporte ou consomme déjà le contexte nécessaire à la matérialisation AOT, identifie précisément ce point et explique pourquoi il doit être touché.

Ne modifie pas `render` simplement parce que le menu finira plus tard par devenir un artefact indépendant consommable par T2A.

---

# 9. Question technique centrale

Déterminer le plus petit mécanisme permettant de passer d'un contexte de représentation de route à une condition AOT.

Conceptuellement :

```text
RouteRepresentationContext
        ↓
template compiler
        ↓
{% if route.current %}
        ↓
branche choisie statiquement
        ↓
Rust généré
        ↓
artefact AOT
```

La session doit d'abord déterminer où ce contexte doit exister :

- parser ;
- AST ;
- resolver ;
- lowering ;
- codegen ;
- page compiler ;
- autre structure déjà existante.

Ne choisis pas une couche avant d'avoir suivi le chemin réel dans le code.

---

# 10. Cas à supporter

Le mécanisme doit permettre au minimum :

### Cas 1 — current

```jinja
{% if route.current %}
A
{% else %}
B
{% endif %}
```

avec `route.current = true`.

Résultat AOT :

```text
A
```

### Cas 2 — non-current

Même template avec :

```text
route.current = false
```

Résultat :

```text
B
```

### Cas 3 — plusieurs représentations

Deux routes différentes doivent pouvoir être compilées avec des contextes différents sans introduire de runtime branching.

### Cas 4 — route sans current

Le contexte doit pouvoir représenter l'absence d'élément courant.

La représentation exacte de cette absence (`None`, valeur symbolique, contexte absent, etc.) est à déterminer.

---

# 11. Attention au nom et à la sémantique

La forme :

```jinja
{% if route.current %}
```

est la forme conceptuelle souhaitée.

Mais ne suppose pas que `current` doit être un simple booléen global.

La sémantique réelle peut devoir permettre :

```text
route.current = Content
```

ou une représentation équivalente, afin qu'un élément de navigation puisse demander si **son** contexte correspond à la route courante.

La session doit donc étudier ce point avant de figer le type.

Ne résous pas artificiellement cette question par une série de booléens si une représentation plus propre existe déjà dans le modèle de compilation.

---

# 12. Aucun runtime conditionnel

Le résultat généré doit être équivalent conceptuellement à :

```rust
// branche sélectionnée pendant la compilation
buf.push_str(...);
```

et non à :

```rust
if route.current {
    ...
} else {
    ...
}
```

dans le chemin d'exécution HTTP.

Le mot « route » dans le template ne doit pas être interprété comme une donnée dynamique du serveur.

---

# 13. Performance et discipline AOT

La nouvelle fonctionnalité ne doit pas introduire :

- interpréteur runtime ;
- allocation runtime liée au template ;
- introspection de route au moment du rendu ;
- moteur de template runtime ;
- parsing HTML runtime ;
- branchement runtime pour `route.current`.

Le coût supplémentaire doit être payé par Forge / génération AOT.

Le hot path HTTP reste inchangé.

---

# 14. Compatibilité avec les mécanismes existants

Le mécanisme doit préserver le fonctionnement actuel de :

```jinja
{% if record.is_readable %}
```

et des autres constructions déjà supportées.

Il faut éviter de créer un second moteur conditionnel parallèle.

Si possible, la bonne architecture est :

```text
expression existante
        ↓
résolution de sa provenance
        ↓
condition AOT
        ↓
lowering/codegen existant
```

avec seulement l'ajout de la nouvelle provenance `route`.

---

# 15. Impact architectural attendu

L'objectif immédiat est de permettre à la navigation de devenir, ultérieurement, une représentation AOT indépendante et réellement exploitable par T2A.

Mais cette session ne doit pas implémenter cette indépendance physique si elle n'est pas nécessaire au mécanisme de contexte de route.

Ne crée pas maintenant :

- `SourceKey` de navigation ;
- `SegmentDescriptor` ;
- `RouteDescriptor` ;
- catalogue de sources ;
- route registry T2A ;
- artefact T2A ;
- Body Hyper ;
- Bytes ;
- `ResolvedRange`.

Ces éléments appartiennent à la session T2A ultérieure.

---

# 16. ADR-008 / ADR-011

Les principes suivants restent intacts :

- `.marius` est AOT ;
- la composition AOT est effectuée à l'écriture ;
- le runtime ne reconstruit pas le HTML ;
- le runtime ne connaît pas la sémantique du menu ;
- route contextualization ≠ Volatile ;
- navigation contextualisée ≠ browser augmentation ;
- JavaScript ne constitue pas une dépendance fonctionnelle ;
- aucune nouvelle runtime fragment abstraction.

Il est possible qu'une **future** évolution d'ADR-008 soit nécessaire pour formaliser la factorisation physique de la navigation.

Ne modifie pas ADR-008 dans cette session sans démontrer précisément pourquoi le mécanisme Forge impose cette modification.

Le mécanisme de contexte AOT et la décision de factorisation physique sont deux questions distinctes.

---

# 17. État Phase 5

La Phase 5 Hyper/Axum compile réellement dans le dépôt.

Cette information est désormais factuelle pour la suite.

Elle n'a toutefois aucune conséquence sur le scope de cette session.

---

# 18. Méthode de travail demandée

Avant toute modification :

1. inspecter le lexer ;
2. inspecter les tokens ;
3. inspecter parser / resolver / validator ;
4. inspecter lowering/codegen ;
5. inspecter le page compiler ;
6. suivre un exemple existant de `{% if record.is_readable %}` jusqu'au Rust généré ;
7. identifier le point minimal où injecter le contexte AOT de route ;
8. vérifier comment les pages/routes sont actuellement connues de Forge ;
9. seulement ensuite proposer le changement.

Ne devine pas les interfaces absentes.

Si une information nécessaire est hors des fichiers actuellement disponibles, indique précisément le fichier ou module à inspecter.

---

# 19. Livrable de cette session

Première étape : **audit et proposition**, pas implémentation immédiate.

Retourner :

1. chaîne actuelle de compilation d'une condition `{% if ... %}` ;
2. origine actuelle des valeurs accessibles au template ;
3. endroit exact où un `RouteRepresentationContext` pourrait être introduit ;
4. représentation conceptuelle de `route.current` ;
5. méthode permettant de sélectionner la branche AOT ;
6. impact sur parser / resolver / lowering / codegen ;
7. fichiers `fragment-forge` réellement à modifier ;
8. éventuels fichiers `core/schema` nécessaires ;
9. éventuels fichiers `shell/render` réellement nécessaires — seulement s'ils le sont ;
10. tests nécessaires ;
11. impact éventuel sur le Guide `.marius` ;
12. risques ou contradictions architecturales ;
13. proposition d'implémentation minimale.

**Ne commence pas l'implémentation avant d'avoir produit cette analyse.**

---

# 20. Critère de réussite

À terme, nous voulons pouvoir écrire dans un template :

```jinja
{% if route.current %}
    représentation de l'élément courant
{% else %}
    représentation de l'élément non courant
{% endif %}
```

et obtenir deux sorties AOT différentes selon le contexte de route de compilation, sans aucune donnée `route.current` nécessaire au runtime.

Le futur T2A pourra alors consommer l'artefact produit sans connaître son origine sémantique.

La séparation fondamentale est :

```text
FORGE
  route context
       ↓
  AOT representation
       ↓
  artefact

T2A
  artefact
       ↓
  ResolvedRange[]
       ↓
  Bytes
       ↓
  Body
       ↓
  Hyper
```

Cette frontière doit rester intacte.