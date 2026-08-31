# ADR-003 — Alignement du Trait `Projection`, Transport des Varlena et Invariants de Thread

**Statut** : Accepté  
**Date** : 2026  
**Composants** : `crates/core/projection`, `crates/core/schema`, `crates/forge/fragment-forge`

---

## Contexte

Le pipeline de projection opère sur deux catégories de données structurellement distinctes :

- **Fixed-length** : types scalaires (`i32`, `i64`, `bool`, `[u8; 16]`). Layout prévisible à la compilation. Représentés par une struct `#[repr(C)]` générée par DB-Forge, alignée bit-à-bit sur le heap tuple PostgreSQL.
- **Varlena** : types à largeur variable (`TEXT`, `VARCHAR(N)`). Taille connue uniquement après désérialisation SQLx. Incompatibles avec `repr(C)` : un fat pointer `String` (16 octets, pointeur + longueur) brise la symétrie binaire avec le DDL.

Le Dispatcher distribue les enregistrements sur des threads via `rayon::par_iter()` et `tokio::spawn()`. Ces deux primitives imposent la contrainte `Send + 'static` sur les types transportés. Un type portant des références empruntées (`&'a str`) n'est pas `'static` et ne peut pas traverser ces frontières.

**Phase 2 (mmap / POSIX shared memory)** : `fetch_batch` sera remplacé par un lecteur d'offsets mémoire partagée. La signature du trait de rendu doit rester stable lors de cette transition.

---

## Problématique

### Option écartée : GAT `type Payload<'a>`

```rust
// NON RETENU
pub trait Projection {
    type Payload<'a>: Send;
    fn render(record: &Self::Record, payload: &Self::Payload<'_>, buf: &mut String);
}
```

**Coûts :**

- Les bounds GAT se propagent sur tout code générique dépendant du trait (`Dispatcher<P: Projection>`), produisant des contraintes `for<'a> P::Payload<'a>: Send` difficiles à satisfaire mécaniquement.
- `Payload<'a>` n'est pas `'static` → ne traverse pas `tokio::spawn` ni `rayon::par_iter` sans contorsion (`Arc`, `unsafe`).
- Introduit un type associé dans l'interface publique pour un artefact qui est un détail d'implémentation interne à `render()`.
- Blocage Phase 2 : en mmap, le payload emprunterait depuis une région `'static`. Le GAT forcerait une implémentation distincte du trait pour gérer le cas `'static`.

### Option écartée : struct `RenderPayload<'a>` dans le fichier généré

Même problème de non-traversabilité des threads. De plus, la struct générée expose un lifetime dans l'interface publique du module généré, couplant le site d'appel à la durée de vie des données SQLx.

---

## Décision

### 1. Séparation `Record` / `VarlenOwned` dans le trait

```rust
pub trait Projection: Sized + Send + Sync + 'static {
    /// Layout fixed-length, #[repr(C)], miroir du heap tuple PostgreSQL.
    /// Send + 'static : traversée tokio::spawn et rayon::par_iter sans contrainte.
    type Record: Sized + Send + 'static;

    /// Données varlena possédées (Option<String>).
    /// Send + 'static : même invariant que Record.
    /// () pour les tables sans varlena : ZST, éliminé à la compilation.
    type VarlenOwned: Sized + Send + 'static;
    ...
}
```

`Record` et `VarlenOwned` sont tous deux `'static`. Le couple `(Record, VarlenOwned)` traverse les frontières de thread sans restriction.

### 2. Signature `fetch_batch`

```rust
fn fetch_batch(
    pool: &sqlx::PgPool,
    ids:  &[i64],
) -> impl std::future::Future<
    Output = Result<Vec<(Self::Record, Self::VarlenOwned)>, sqlx::Error>
> + Send;
```

Le retour est un `Vec` de tuples possédés. `impl Future + Send` est explicite : `async fn` dans un trait public ne contraint pas `Send` sur le `Future` retourné, ce qui bloque `tokio::spawn`.

**Phase 2** : l'implémentation substituera `sqlx::query_as` par un lecteur d'offsets mmap. La signature du trait reste inchangée ; seul le corps de l'implémentation générée change.

### 3. `type VarlenOwned = ()` pour les tables sans varlena

`()` est un ZST (_Zero-Sized Type_). Le compilateur l'élimine entièrement : aucun octet alloué, aucune instruction émise pour le paramètre `varlena` dans `render()`. Le code appelant utilise `&()` sans overhead.

### 4. Signature `render` et reconstruction locale du payload

```rust
fn render(record: &Self::Record, varlena: &Self::VarlenOwned, buf: &mut String);
```

Fragment-Forge génère en tête du corps de `render()` :

```rust
// Reconstruction locale — durée de vie inférée depuis `varlena`, jamais exposée.
let headline_ref:             Option<&str> = varlena.headline.as_deref();
let description_ref:          Option<&str> = varlena.description.as_deref();
let alternative_headline_ref: Option<&str> = varlena.alternative_headline.as_deref();
```

**Invariants :**

- `as_deref()` : réaffectation de fat pointer (`*const u8` + `usize`). Zéro copie de bytes.
- Le lifetime des `&str` est lié à `varlena` (paramètre de `render()`), inféré par le compilateur. Il n'apparaît dans aucune interface publique.
- Ces références ne traversent aucune frontière de thread : elles naissent et meurent dans `render()`.
- `buf` est pré-alloué à `STATIC_CAP + DYNAMIC_CAP` avant l'appel. `render()` ne déclenche aucun `realloc`.

### 5. Invariant d'allocation du Dispatcher — $O(T)$ vs $O(N)$

Pour éliminer l'amplification d'allocation sur le chemin critique, le Dispatcher interdit la création de buffers de rendu à l'échelle de l'enregistrement ($O(N)$, une allocation par record). Le pipeline de rendu parallèle utilise le pattern de distribution par thread de Rayon (`map_with`) pour allouer une unique `String` par cœur CPU logique ($O(T)$, $T$ = nombre de threads Rayon) :

```rust
records.into_par_iter().map_with(String::new(), |buf, (record, varlena)| {
    buf.clear();
    P::render(&record, &varlena, buf);
    // écriture de l'artefact...
}).for_each(|_| {});
```

`map_with` distribue une copie du seed `String::new()` (capacité 0) à chaque thread au démarrage du pool Rayon. `.clear()` remet `len = 0` sans libérer la mémoire allouée sur le tas : dès le premier `render()`, le buffer atteint `STATIC_CAP + DYNAMIC_CAP` et se stabilise. Les itérations suivantes du même thread n'allouent plus. L'empreinte RAM du Dispatcher est bornée par $T \times \text{TOTAL\_CAP}_{\max}$, indépendamment de $N$.

### 6. Déstructuration complète de `Row` dans `fetch_batch` (correctif E0382)

La conversion `Row → (StorageRow, VarlenOwned)` ne peut pas s'écrire :

```rust
// INVALIDE — partial move E0382
let owned = VarlenOwned { field: r.field };  // déplace r.field
let storage = StorageRow::from(r);           // r partiellement déplacé
```

Le générateur émet une déstructuration complète en un seul pattern `let` :

```rust
let StructRow { fixed_field_1, fixed_field_2, varlena_field, .. } = r;
let owned   = VarlenOwned   { varlena_field };
let storage = StorageRow    { fixed_field_1, fixed_field_2: fixed_field_2.unwrap_or(0), .. };
```

La clause `..` absorbe les champs non nommés (Phase 2, types inconnus) sans les déplacer. `From<Row> for StorageRow` n'est pas appelé dans ce chemin ; la logique de conversion (sentinels, `timestamp_micros()`) est reproduite inline depuis `map_type()`.

---

## Conséquences

| Invariant                             | Mécanisme                                                      |
| ------------------------------------- | -------------------------------------------------------------- |
| Pas d'allocation sur le hot path      | `buf` pré-alloué, `as_deref()` sans copie                      |
| Traversée de thread sans restriction  | `Record + VarlenOwned : Send + 'static`                        |
| Aucun GAT dans l'interface publique   | `RenderPayload<'a>` local à `render()`                         |
| Stabilité Phase 2                     | Seul le corps de `fetch_batch` change                          |
| Tables sans varlena : coût zéro       | `type VarlenOwned = ()` (ZST)                                  |
| Détection précoce des divergences DDL | `static_assert!(size_of, align_of)` dans `StorageRow`          |
| Empreinte RAM Dispatcher bornée       | $O(T)$ allocations — `map_with`, une `String` par thread Rayon |

**Calibration du pire cas (_Worst-Case Execution Space_).** Le calcul de `DYNAMIC_CAP` par la Forge est structurellement pessimiste. Pour les colonnes à largeur variable non bornées (`TEXT`), la Forge applique un fallback conservateur multiplié par le facteur d'échappement HTML maximal ($\times 5$). En conséquence, le ratio d'occupation réelle du buffer sur des payloads nominaux courts peut s'effondrer aux alentours de 5%. Ce comportement est nominal et accepté : l'invariant de sécurité absolu est la garantie de non-réallocation au runtime, et non la densité de remplissage du buffer. Le seuil de validation statique est fixé à $> 3\%$ pour absorber les colonnes `TEXT` larges sans déclencher de faux positifs dans la suite de tests.

**Dette technique identifiée** : la logique de conversion `map_type()` est dupliquée entre `write_from_impl` et la déstructuration inline dans `write_projection_stub`. Toute modification de `map_type()` doit être répercutée dans les deux sites. Résolution cible : extraire un helper `emit_field_conversion(col, binding_name)` dans DB-Forge.
