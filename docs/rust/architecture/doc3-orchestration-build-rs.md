# Document 3 — Orchestration (`build.rs`)

**Contrat d'architecture.** Portée : `crates/core/schema/build.rs`, fonction `resolve_template` et son voisinage direct. Objectif : montrer que l'intégration du Mode Page ne modifie ni la signature externe de `resolve_template`, ni une seule ligne du Resolver (`validate_ast`, `resolve_and_measure`) ni du Codegen (`generate_aot_snippet`).

---

## 1. Point exact de détection du mode

`resolve_template` lit déjà la source (`std::fs::read_to_string`) avant tout appel à `scan()`. Le branchement s'insère **immédiatement après cette lecture, avant le premier appel à `scan()`** :

```
lecture du fichier .marius (I/O, déjà existant)
        │
        ▼
detect_extends(&src)  ◀── point de branchement, aucune autre position n'est acceptable :
        │                  toute lecture ultérieure (scan, parse) présuppose déjà
        │                  un choix de grammaire.
   ┌────┴────┐
 false      true
   │          │
   ▼          ▼
Mode Fragment  Mode Page
(inchangé)     (Document 1 + Document 2)
```

Aucune variable d'environnement, aucune convention de nommage de fichier : le mode est une propriété du **contenu**, pas du chemin. Conséquence directe : un même appelant (`main()`, boucle sur les composants) n'a besoin d'aucune connaissance du mode — `resolve_template` absorbe entièrement la décision.

---

## 2. Graphe complet des appels

```
main()
  └─ pour chaque composant (schema, table) :
       resolve_template(manifest_dir, schema, table, &field_specs, &varlena)
         │
         ├─ lecture template_path (I/O)
         ├─ detect_extends(&src)
         │
         ├─ [false] chemin Mode Fragment (INCHANGÉ)
         │     scan(&src)
         │       → parse_tokens(spans) ──────────────────┐
         │                                               │
         └─ [true]  chemin Mode Page (NOUVEAU)           │
               read_template_file(parent_path)  (I/O)    │
               scan(&child_src) → parse_page_tokens ─────┤ (Document 1)
               scan(&parent_src) → parse_page_tokens ────┤ (Document 1)
               vérifier parent.extends == None           │ (garde v1, §5)
               arena.admit(child) → child_id             │
               arena.admit(parent) → parent_id           │ (Document 2 §2)
               collect_blocks(child_id, …)  ────┐        │
               collect_blocks(parent_id, …) ────┤        │ (Document 2 §3)
               collect_static_refs(…)          ─┤        │
               link(parent_blocks, child_blocks,│        │
                    static_refs, file_exists) ──┤        │ (Document 2 §4)
               lower(parent.tokens, plan, arena)┘        │
                     → Vec<FlatPageToken<'src>> ─────────┘
         │
         ▼
     (POINT DE JONCTION — un seul chemin à partir d'ici, quel que soit le mode)
         validate_ast(&tokens)               ◀── gelé, inchangé
         resolve_and_measure(&mut tokens,
             &schema_index, get_file_size)   ◀── gelé, inchangé
         generate_aot_snippet(&tokens,
             &schema_index)                  ◀── gelé, inchangé
         │
         ▼
     Ok(Some((body, metrics)))  ──▶ write_projection_stub (db-forge, inchangé)
```

**Point de jonction unique** : dès l'obtention d'un `Vec<FlatPageToken<'src>>` — que ce soit en sortie directe de `parse_tokens` (Fragment) ou de `lower` (Page) — la suite du graphe est strictement identique. C'est la preuve opérationnelle de la convergence : aucune fonction gelée ne reçoit de paramètre supplémentaire, aucun `match` sur un « mode » ne réapparaît après ce point.

---

## 3. Responsabilités par étape

| Étape                  | Responsabilité                                     | I/O                   | Nouveau ?                              |
| ---------------------- | -------------------------------------------------- | --------------------- | -------------------------------------- |
| `detect_extends`       | Discriminant de mode                               | Non                   | Oui (Document 1)                       |
| Lecture parent         | Suivre `extends`, charger le fichier référencé     | Oui                   | Oui — **seule E/S nouvelle du build**  |
| `parse_page_tokens` ×2 | AST mono-fichier (enfant, parent)                  | Non                   | Oui (Document 1)                       |
| Garde single-level     | Rejeter un parent qui déclare lui-même `extends`   | Non                   | Oui — décision de portée, voir §5      |
| `PageArena::admit` ×2  | Identité stable (`TemplateId`)                     | Non                   | Oui (Document 2 §2)                    |
| `collect_blocks` ×2    | Plages de blocs + validation de forme schéma-libre | Non                   | Oui (Document 2 §3)                    |
| `collect_static_refs`  | Extraire les `StaticPartialRef` des deux fichiers  | Non                   | Oui — utilitaire, signature ci-dessous |
| `link`                 | Correspondance blocs, existence des `static`       | Oui (`file_exists`)   | Oui (Document 2 §4)                    |
| `lower`                | Fusion → `Vec<FlatPageToken>`                      | Non                   | Oui (Document 2 §5)                    |
| `validate_ast`         | Gate sémantique (bornes if, champs)                | Non                   | **Non — gelé**                         |
| `resolve_and_measure`  | Résolution taille + capacité                       | Oui (`get_file_size`) | **Non — gelé**                         |
| `generate_aot_snippet` | Émission Rust                                      | Non                   | **Non — gelé**                         |

Seules trois familles d'E/S existent dans tout le graphe : lecture des fichiers `.marius` (enfant + parent), vérification d'existence des fichiers `static` (Linker), lecture de taille des fichiers `include`/`static` (Resolver, gelé). Aucune autre fonction du graphe ne touche le disque — cohérent avec le principe déjà acté : `build.rs` concentre l'intégralité de l'E/S, la Forge reste pure partout ailleurs.

---

## 4. Signatures attendues (build.rs, non `pub` — binaire)

```rust
/// Lecture brute d'un fichier .marius. Utilisée pour l'enfant (déjà
/// existant) et pour le parent (nouveau, même fonction réutilisée).
fn read_template_file(path: &Path) -> Result<String, ()>;

/// Point d'entrée mis à jour. Signature EXTERNE inchangée par rapport
/// à l'existant — seul le corps se branche en interne.
fn resolve_template(
    manifest_dir: &str,
    schema:       &str,
    table:        &str,
    fixed:        &[FieldSpec],
    varlena:      &[VarlenField],
) -> Result<Option<(String, TemplateMetrics)>, ()>;

/// Sous-orchestration Mode Page, appelée uniquement si detect_extends
/// est vrai. Isolée dans sa propre fonction pour ne pas alourdir le
/// chemin Mode Fragment existant d'un seul branchement supplémentaire
/// visible dans son corps.
fn resolve_page_template<'src>(
    manifest_dir: &str,
    schema:       &str,
    table:        &str,
    fixed:        &[FieldSpec],
    varlena:      &[VarlenField],
    child_src:    &'src str,
    child_extends: &'src str,
) -> Result<(String, TemplateMetrics), ()>;

/// Utilitaire d'extraction, support du Linker (Document 2 §4) — ne
/// modifie aucun type déjà scaffoldé, se contente de filtrer.
fn collect_static_refs<'src>(
    tokens: &[PageSourceToken<'src>],
) -> Vec<StaticPartialRef<'src>>;
```

`resolve_page_template` retourne `Result<(String, TemplateMetrics), ()>` (non `Option` — appelée seulement quand un template existe et déclare `extends`, l'absence de fichier n'est pas un cas de cette fonction, elle est déjà tranchée par l'appelant avant le branchement §1).

---

## 5. Garde single-level et rerun-if-changed

Deux ajouts factuels au corps de `resolve_template`/`resolve_page_template`, nécessaires pour que le graphe reste correct, non couverts par un document précédent :

- **Garde single-level** : après `parse_page_tokens` du parent, si `parent.extends.is_some()`, `cargo:error` explicite (« héritage multi-niveaux non supporté ») puis `exit(1)` — cohérent avec le point ouvert du Document 2 §6.1 : la garde est posée à l'orchestration, pas dans le typage.
- **Invalidation de cache** : `println!("cargo:rerun-if-changed={parent_path}")` doit être émis pour le fichier parent, en plus du fichier enfant déjà couvert. Un `base.marius` modifié sans que le fichier de la table ne change doit invalider le build — omission silencieuse sinon.

---

## 6. Coût accepté, non optimisé (v1)

Chaque table qui `extends` un même `base.marius` déclenche sa propre lecture, son propre parsing, sa propre arène — aucun cache inter-tables. Accepté pour ce contrat : la boucle `for comp in &components` (`main()`, inchangée) reste indépendante d'un composant à l'autre, ce qui garantit qu'aucun état partagé n'a besoin d'être géré entre deux itérations. Une session ultérieure peut introduire un cache `chemin → ParsedPageTemplate` à l'échelle du build si le coût de reparsing devient mesurable — hors périmètre ici, et volontairement : l'ajouter maintenant introduirait un état mutable partagé entre itérations, un risque architectural que ce contrat n'a pas à porter avant d'en avoir la preuve par la mesure.

---

## 7. Ce qui ne change pas — récapitulatif de clôture

- Signature externe de `resolve_template` : inchangée.
- `validate_ast`, `resolve_and_measure`, `generate_aot_snippet` : zéro modification de signature, zéro modification de corps.
- `write_projection_stub` (db-forge) : reçoit toujours `Option<(&str, &TemplateMetrics)>` déjà calculé — ignore totalement l'existence du Mode Page.
- Boucle `main()` sur les composants : inchangée, aucune connaissance du mode.

Le Mode Page est, du point de vue de tout le reste du système, un chemin de production alternatif d'une seule valeur : `Vec<FlatPageToken<'src>>`. Rien en aval de cette valeur ne sait qu'elle a pu naître d'une fusion parent/enfant plutôt que d'un fichier unique.

---

_2 juillet 2026_
