# Contrat d'Implémentation — Injection Varlena Brute (`raw`)

**Fonde sur** : constat de session (22/07/2026) — le panic `max_escaped_len (1200000B)
> 64 KB` sur `content.body.content` n'est pas un problème de seuil, mais la
conséquence de deux défauts cumulés :

1. `HTML_ESCAPE_FACTOR = 6` (`introspect.rs` ligne 288) est appliqué à un champ qui
   n'est **pas** du texte à échapper — `content.body.content` est du HTML déjà
   constitué, destiné à être injecté tel quel.
2. `pre_escaped` (`VarlenField`, `fragment-forge/lib.rs`) ne règle qu'à moitié ce
   cas : il ramène le facteur de capacité à 1, **mais le générateur appelle
   `marius_html_escape(s, buf)` inconditionnellement pour tout champ varlena**
   (`lib.rs` ligne 2296-2304, confirmé par lecture directe), `pre_escaped` ou non.
   Un champ HTML tagué `pre_escaped` verrait donc ses balises échappées quand
   même au runtime — bug silencieux, pas une simple sous-estimation de capacité.

`pre_escaped` reste correct pour son cas d'usage documenté (texte normalisé, sans
caractères spéciaux — slugs, titres). Il est **sémantiquement faux** de l'utiliser
pour `content.body.content`, qui contient au contraire énormément de `<`/`>`/`&`
intentionnels. Un état distinct est nécessaire : **`raw`** — contenu qui ne doit
JAMAIS être échappé, quel que soit son contenu réel.

**Discipline d'exécution** : identique aux contrats précédents — étapes atomiques,
testées isolément, fichiers non vus à demander avant d'écrire.

**Point d'arbitrage bloquant avant l'Étape 2** : forme du nouvel état sur
`VarlenField` — voir Étape 2.

---

### Étape 1 — Tag SQL `marius:raw` + détection dans `introspect.rs`

**Crate** : `crates/forge/db-forge`, `introspect.rs`.
**Contenu** : à côté de la détection existante (ligne 209,
`let pre_escaped = description.trim() == "marius:pre_escaped";`), ajouter
`let raw = description.trim() == "marius:raw";`. Mutuellement exclusif de facto
(un `description` ne peut être égal qu'à une seule chaîne à la fois) — aucune
garde supplémentaire nécessaire pour l'instant, mais à surveiller si la détection
évolue un jour vers un format multi-tags.
**Dépend de** : rien.
**Fichier à confronter avant d'écrire** : aucun nouveau — `introspect.rs` déjà en
main.
**Critère de complétion** : test unitaire — colonne avec commentaire
`'marius:raw'` → `raw == true`, `pre_escaped == false` ; commentaire
`'marius:pre_escaped'` → inchangé ; commentaire absent/autre → les deux `false`.

### Étape 2 — Nouvel état sur `VarlenField` (ARBITRAGE REQUIS)

**Crate** : `crates/forge/fragment-forge`, `lib.rs`.
**Deux formes possibles, non tranchées ici** :
- **(a) Champ additif** `pub raw: bool`, à côté de `pre_escaped: bool` existant.
  Moins invasif — ne casse que les sites de construction déjà connus (Étape 2 du
  Contrat multi-slot les a déjà tous recensés : `introspect.rs`, 3 sites de test
  dans `lib.rs`, `varlen()` dans `validate.rs`). Risque résiduel : les deux
  booléens forment un état à 4 combinaisons dont une (`pre_escaped && raw`) n'a
  aucun sens et doit être rejetée explicitement quelque part.
- **(b) Enum fermé** `pub escape_policy: EscapePolicy { Escaped, PreEscaped, Raw }`,
  remplaçant `pre_escaped: bool`. Plus cohérent avec la discipline DDL-stricte
  déjà arbitrée pour la collision de nom (« aucun état invalide représentable »)
  — mais casse la signature partout où `.pre_escaped` est lu aujourd'hui
  (`varlen.rs`, `row.rs`, la fonction `max_escaped_len()` de `VarlenField` elle-même
  ligne ~265).
**Dépend de** : rien de structurel, mais bloque toute écriture ultérieure tant que
non tranché.
**Critère de complétion** : n'est pas définissable avant l'arbitrage.

### Étape 3 — Capacité : facteur 1 pour `raw` (comme `pre_escaped`)

**Crate** : `crates/forge/db-forge`, `introspect.rs` (ligne 283-287).
**Contenu** : `let escape_factor = if pre_escaped || raw { 1 } else { ... }`
(ou équivalent selon la forme retenue à l'Étape 2). Le calcul de capacité pour
`raw` est identique à celui de `pre_escaped` (facteur 1) — la différence entre
les deux n'est **jamais** dans le calcul de capacité, seulement dans le
comportement runtime (Étape 4).
**Dépend de** : Étape 1, Étape 2.
**Critère de complétion** : `content.body.content` (200 000B, tag `raw`) →
`max_escaped = 200 000`, toujours **au-dessus** du seuil 64 Ko à ce stade — la
migration de réduction (Étape 7) reste nécessaire en plus de ce correctif, pas à
sa place.

### Étape 4 — Codegen : ne pas échapper les champs `raw`

**Crate** : `crates/forge/fragment-forge`, `lib.rs` (ligne 2296-2304, fonction de
génération du corps `render()`).
**Contenu** : la branche `if schema.find_varlena(field).is_some()` doit désormais
distinguer `raw` des autres champs varlena — pour un champ `raw`,
émettre `if let Some(s) = {field}_ref {{ buf.push_str(s); }}` (aucun appel à
`marius_html_escape`) ; pour tout autre champ varlena (échappé ou `pre_escaped`),
comportement inchangé (`marius_html_escape(s, buf)`).
**Point à vérifier avant d'écrire** : `schema.find_varlena(field)` retourne
aujourd'hui probablement juste `Option<&VarlenField>` ou équivalent suffisant pour
lire `.raw`/`.escape_policy` directement — à confirmer en relisant `find_varlena`
(non vu dans le fragment de `lib.rs` déjà audité cette session, à localiser avant
d'écrire cette étape).
**Dépend de** : Étape 2.
**Critère de complétion** : test `generate_aot_snippet` (sur le modèle de
`test_generate_aot_snippet_typed`, ligne ~2813) — un champ `raw` produit
`buf.push_str(s)` dans le snippet généré, jamais `marius_html_escape`.

### Étape 5 — Commentaires générés (`varlen.rs`, `row.rs`)

**Crate** : `crates/forge/db-forge`, `codegen/varlen.rs` et `codegen/row.rs`.
**Contenu** : le commentaire généré au-dessus de chaque champ
(`write_varlen_owned_struct`, ligne ~50-60) distingue aujourd'hui seulement
« pré-échappé » vs « escape HTML » — ajouter un troisième libellé pour `raw`
(« brut, jamais échappé — HTML pré-rendu »), cohérent avec la politique déjà en
place de tout documenter explicitement dans le code généré plutôt que de le
masquer (cf. traitement de `max_len: Option<usize>` dans le même fichier).
**Dépend de** : Étape 2.
**Critère de complétion** : code généré pour `content.core` (champ `content`,
tag `raw`) porte un commentaire distinct de `identity.slug`/`identity.headline`
(échappés) et de tout champ `pre_escaped` existant.

### Étape 6 — Non-régression `pre_escaped`

**Contenu** : tout champ `pre_escaped` existant (aucun dans le schéma réel à ce
jour, mais couvert par les tests unitaires de `fragment-forge`) continue
d'échapper au runtime exactement comme avant — seul `raw` change de comportement
runtime, pas `pre_escaped`.
**Dépend de** : Étapes 1 à 5.
**Critère de complétion** : tests existants de `fragment-forge` inchangés, verts.

### Étape 7 — Migration DDL : tag `raw` + réduction de borne

**Contenu** :
1. `COMMENT ON COLUMN content.body.content IS 'marius:raw';`
2. `ALTER TABLE content.body ALTER COLUMN content TYPE VARCHAR(32000);` (ou une
   autre valeur ≤ 65536, avec facteur 1 désormais correct) — même discipline que
   `02_content_body_varlena_bound.sql` (scan de validation pré-migration,
   `content.v_article` à recréer autour de l'`ALTER`, GRANTs réappliqués).
**Dépend de** : Étapes 1 à 6, closes et buildées réellement.
**Critère de complétion** : migration exécutée en conditions réelles, `cargo build`
passe (plus de panic `max_escaped_len`).

### Étape 8 — Validation bout-en-bout : `{{ record.content }}` produit du HTML réel

**Contenu** : reprend le point 4/5 de la checklist `HANDOFF-v2`, cette fois
correctement — le corps d'article s'affiche comme du HTML rendu (balises
interprétées), pas comme texte échappé visible.
**Dépend de** : Étape 7.
**Critère de complétion** : build réel + inspection du HTML produit (pas de
`&lt;`/`&gt;` dans la sortie pour ce champ).

---

## Dépendances entre étapes — résumé

```
1 (tag SQL) ──┬──▶ 3 (capacité facteur 1) ──▶ 6 (non-régression) ──▶ 7 (migration) ──▶ 8
2 (arbitrage) ┴──▶ 4 (codegen no-escape) ──▶ 6
              └──▶ 5 (commentaires générés) ──▶ 6
```

## Point à trancher avant l'Étape 2

Forme (a) champ additif `raw: bool`, ou (b) enum fermé `EscapePolicy` remplaçant
`pre_escaped: bool` — aucune ne peut être déduite du code existant. Arbitrage
requis avant d'écrire la moindre ligne de l'Étape 2.
