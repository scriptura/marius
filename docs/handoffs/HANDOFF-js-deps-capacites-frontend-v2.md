# HANDOFF v2 — `js_deps` / capacités frontend

> Consolide toutes les décisions prises lors de la session de conception qui a
> suivi `HANDOFF-js-deps-capacites-frontend.md` (v1). Ce document **remplace**
> v1 comme référence de reprise — v1 reste la trace historique du problème
> initial, pas la source de vérité courante.
>
> Légende stricte, à respecter par tout agent reprenant ce document :
> - **🟢 Acté** — décision architecturale figée. Ne pas rouvrir sans arbitrage
>   explicite du concepteur.
> - **🟡 Vérification d'implémentation** — fait constaté sur du code réel,
>   ou détail technique non encore vérifié. Peut être complété/corrigé sans
>   remettre en cause l'architecture.
> - **🔴 Décision/donnée encore nécessaire** — bloquant, à trancher avant de
>   pouvoir considérer l'implémentation complète sur ce point précis.
>
> Ne jamais traiter une entrée 🟡 comme si elle était 🔴 — une information
> simplement non encore inspectée n'est pas une question architecturale ouverte.

---

## Schéma global des couches concernées

```text
theme.toml
   │
   ├── [scripts.capabilities.*]
   │       ├── entry       (chemin du fichier JS)
   │       ├── markers      (tokens class= qui activent la capacité)
   │       └── activation   (export ESM nommé, zéro argument)
   │
   ▼
marius-assets (crate autonome, extractible — AUCUNE connaissance de
   │            content.body / content.core / js_deps / PostgreSQL)
   └── résolution/minification/hash du module JS → fichier servi

content.body.content (TEXT, non canonicalisé)
   │
   ▼  trigger BEFORE INSERT OR UPDATE OF content
compute_js_deps(NEW.content)  ── écrit dans content.core.js_deps (cross-table)
```

Deux branches **structurellement disjointes**, jointes uniquement par le fait
que les deux lisent (l'une au build, l'autre à la main) le même vocabulaire
de capacités déclaré dans `theme.toml`. Aucun outil ne relie automatiquement
les deux — voir 🟢 « Provisionnement SQL » ci-dessous.

---

## 🟢 Acté

### Nommage et structure de configuration
- Section `theme.toml` : **`[scripts.capabilities.*]`** (pas `dependencies` —
  rejeté pour ambiguïté avec les dépendances ESM internes à un module et avec
  les dépendances *entre* capacités, elles-mêmes rejetées ci-dessous).
- Structure Rust (`config.rs`), à ajouter à `ScriptsConfig` existant sans
  toucher aux champs actuels :
  ```rust
  #[derive(Deserialize, Default)]
  pub(crate) struct ScriptsConfig {
      #[serde(default)]
      pub(crate) components: HashMap<String, String>,
      #[serde(default)]
      pub(crate) capabilities: HashMap<String, CapabilityConfig>,
  }

  #[derive(Deserialize)]
  pub(crate) struct CapabilityConfig {
      pub(crate) entry: String,
      pub(crate) markers: Vec<String>,
      pub(crate) activation: String,
  }
  ```
- Clé de la `HashMap` = nom de capacité = future clé d'attribution de bit.
  Pas de champ `name` redondant.
- **Aucun champ `depends_on`/graphe entre capacités.** Chaque capacité est
  indépendante ; le tri topologique de capacités a été explicitement rejeté.

### Contrat d'activation JS
- `activation` désigne **exclusivement un export ESM nommé, explicitement
  appelable, zéro argument**.
- Aucun mode `window.*`, IIFE globale, ou « activation implicite » n'est
  ajouté au contrat — même si des modules legacy fonctionnent aujourd'hui
  sur ce mode (voir 🟡 plus bas : ce sont des données de migration, pas une
  extension du contrat).
- `youtube-embed` et `.progress`/`_base.js` sont **hors périmètre** tant
  qu'ils n'ont pas été migrés en modules ESM conformes. Le contrat Marius ne
  bouge pas pour accommoder leur état actuel.

### Vocabulaire frontend legacy = donnée de migration, jamais une contrainte
- Toute lecture du code JS existant (`map.js`, `disclosure.js`, etc.) sert à
  **vérifier** la conformité au contrat déjà défini, jamais à en déduire une
  extension du contrat. Principe appliqué de façon répétée cette session,
  à reconduire pour toute capacité restante.

### Leaflet
- Option retenue : `import "leaflet.js";` ajouté en interne à `map.js` (pas
  de bloc `{% script %}` dédié dans `base.marius`).
- **Fait confirmé sur pièce** : le bloc `{% script %}` de `leaflet.js` dans
  `base.marius` est déjà commenté (HTML comment), retiré manuellement en
  anticipation de `js_deps`. Ne pas le décommenter par mégarde lors de
  l'implémentation.

### Architecture `js_deps` côté SQL
- **Fonction PL/pgSQL avec bits câblés en dur** (« option B »), pas une table
  de définitions lue à l'écriture (« option A », rejetée — introduirait un
  mini-interpréteur runtime, contraire à l'invariant AOT du projet).
- **Provisionnement — Branche 1, confirmée** : il n'existe **aucune autorité
  de génération SQL** dans ce projet (`db/*.sql` est intégralement écrit à la
  main, déployé par `psql -f ...`, cf. `runtime-lifecycle-guide.md` §3 —
  même régime que les triggers `NOTIFY` déjà en place). `compute_js_deps`
  suit exactement cette convention : **fichier `.sql` écrit à la main**,
  synchronisé manuellement avec `[scripts.capabilities.*]`. Pas de pipeline
  `theme.toml → Forge → SQL`. `marius-assets` reste un compilateur d'assets
  autonome et extractible, sans aucune connaissance du domaine `content`.
  Une éventuelle vérification croisée outillée (`theme.toml` ↔ SQL) est
  **explicitement différée**, pas engagée cette session (voir 🔴).
- L'invariant *« `js_deps` est une fonction pure et déterministe de
  `content.body.content` »* est **porté par un trigger**, jamais par les
  procédures appelantes. Aucune procédure d'édition ne doit dupliquer ce
  calcul.

### Procédure d'édition canonique + trigger
- `content.edit_document_body(p_document_id INT, p_content TEXT)` — seul
  chemin métier pour muter `content.body`. UPSERT
  (`ON CONFLICT (document_id) DO UPDATE`, sécurisé par la `PRIMARY KEY`
  existante de `content.body`). Garde RLS identique au patron déjà établi
  (`save_revision`, etc.) : `edit_contents` (bit 4) ou `edit_others_contents`
  (bit 32768) + vérification d'ownership via `content.core.author_entity_id`.
  Met à jour `content.core.modified_at`. **Ne connaît pas `js_deps`.**
  ```sql
  CREATE PROCEDURE content.edit_document_body(p_document_id INT, p_content TEXT)
  LANGUAGE plpgsql AS $$
  BEGIN
    IF identity.rls_user_id() <> -1 THEN
      IF (identity.rls_auth_bits() & 4) <> 4
         AND (identity.rls_auth_bits() & 32768) <> 32768 THEN
        RAISE EXCEPTION 'insufficient_privilege: edit_contents or edit_others_contents required'
          USING ERRCODE = '42501';
      END IF;
      IF (identity.rls_auth_bits() & 32768) <> 32768 THEN
        PERFORM 1 FROM content.core
        WHERE document_id = p_document_id AND author_entity_id = identity.rls_user_id();
        IF NOT FOUND THEN
          RAISE EXCEPTION 'insufficient_privilege: cannot edit another author''s document'
            USING ERRCODE = '42501';
        END IF;
      END IF;
    END IF;

    INSERT INTO content.body (document_id, content)
    VALUES (p_document_id, p_content)
    ON CONFLICT (document_id) DO UPDATE SET content = EXCLUDED.content;

    UPDATE content.core SET modified_at = now() WHERE document_id = p_document_id;
  END;
  $$;
  ```
- Trigger `content.body → content.core.js_deps` — **écriture cross-table**
  (topologie réelle : `js_deps` vit sur `content.core`, la source sur
  `content.body`, deux tables distinctes reliées par `document_id`) :
  ```sql
  CREATE OR REPLACE FUNCTION content.fn_recompute_js_deps()
  RETURNS TRIGGER
  LANGUAGE plpgsql
  SECURITY DEFINER
  SET search_path = content, pg_temp
  AS $$
  BEGIN
    UPDATE content.core
    SET js_deps = content.compute_js_deps(NEW.content)
    WHERE document_id = NEW.document_id;
    RETURN NEW;
  END;
  $$;

  DROP TRIGGER IF EXISTS trg_content_body_js_deps ON content.body;
  CREATE TRIGGER trg_content_body_js_deps
  BEFORE INSERT OR UPDATE OF content ON content.body
  FOR EACH ROW EXECUTE FUNCTION content.fn_recompute_js_deps();
  ```

### Contrat du marqueur de détection — 🟢 fermé
- Source : `content.body.content`. **Non garanti provenir exclusivement des
  templates `.marius`** — chemin d'écriture directe existe
  (`edit_document_body` accepte du `TEXT` brut, sans validation de forme) ⇒
  HTML potentiellement non canonicalisé.
- Marqueur = **token exact d'un attribut `class`**, délimiteur `'` **ou**
  `"` tous deux légitimes (pas de contrainte artificielle imposée à une
  donnée dont la forme n'est pas garantie).
- Tokenisation sur espace HTML usuel (`\s+` : espace, tabulation, retour
  ligne — pas seulement l'espace simple).
- **Aucune** sous-chaîne, préfixe, attribut `data-*`, attribut booléen, ou
  parsing HTML structurel général. Le calcul reste un test d'appartenance
  ensembliste borné, jamais un analyseur HTML généraliste.
- Fonction de référence (bits `disclosure`/`map` confirmés, reste à
  compléter — voir 🔴) :
  ```sql
  CREATE OR REPLACE FUNCTION content.compute_js_deps(p_body TEXT)
  RETURNS INT8 LANGUAGE plpgsql AS $$
  DECLARE
    v_tokens TEXT[];
    v_deps   INT8 := 0;
  BEGIN
    SELECT COALESCE(array_agg(DISTINCT token), '{}')
    INTO v_tokens
    FROM (
      SELECT unnest(regexp_split_to_array(trim(COALESCE(m[1], m[2])), '\s+')) AS token
      FROM regexp_matches(p_body, 'class=(?:"([^"]*)"|''([^'']*)'')', 'g') AS m
    ) t
    WHERE token <> '';

    IF v_tokens && ARRAY['tabs','accordion'] THEN v_deps := v_deps | 1; END IF;
    IF v_tokens && ARRAY['map']              THEN v_deps := v_deps | 2; END IF;
    -- capacités restantes : voir 🔴, ne pas compléter par extrapolation

    RETURN v_deps;
  END;
  $$;
  ```

### Vocabulaire du corpus
- Ne pas employer « hack architectural » pour désigner `<!-- MARIUS_MODULES -->`.
  Terme retenu : **point d'extension textuel post-abaissement** — il permet
  de faire évoluer le pipeline assets/modules sans modifier le contrat de
  `FlatPageToken` (AST gelé, cf. `fragment-forge-lib.rs` : `#[repr(C)]`,
  zéro branchement dans le template généré).

---

## 🟡 Vérification d'implémentation

- Exports ESM confirmés par lecture externe (non revérifiés directement par
  l'agent de conception), **traités comme données de migration** :
  - `map.js` → `initMaps` — conforme au contrat.
  - `disclosure.js` → `initDisclosureSystem` — conforme.
  - `imageFocus.js` → plusieurs exports (`setSystemState`, `decorateTargets`,
    `init`) ; `init` retenu comme `activation` — conforme (le contrat
    n'interdit pas les exports additionnels non désignés).
  - `mediaPlayer.js` → plusieurs exports (`timeStore`, `statusStore`, ...,
    `bootstrap`) ; `bootstrap` retenu — conforme.
  - `lineMark.js` → `boot` — conforme.
  - `range.js` → ambigu entre `mountAllSliders` et `setupSliderEvents` comme
    point d'entrée logique — **non tranché**, à revoir si/quand cette
    capacité est intégrée.
  - `youtube.js` → aucun export ESM (IIFE, `window.YouTubePlayer.bootstrap`)
    — migration requise avant intégration à `js_deps` (🟢 acté : hors
    périmètre tant que non migré).
  - `_base.js` → aucun export ESM (IIFE auto-invoquée) — hors périmètre,
    rien à en déduire (`.progress` non traité).
- Comportement réel de `run_scripts_pipeline` pour un `entry` de capacité
  (vs `components`) : **non vérifié en détail** cette session. Probablement
  inchangé du patron `components` existant, à confirmer.
- Mécanisme de résolution de `import "leaflet.js"` (import non-relatif) au
  sein de `map.js` par le pipeline `marius-assets` : non détaillé cette
  session.
- Contenu réel de `crates/forge/db-forge/src/*` : seuls les noms de fichiers
  sont connus (`tree.md`) et le commentaire de `schema-lib.rs` (« Inclusion
  du code généré par DB-Forge + Fragment-Forge »). La direction SQL→Rust est
  déduite de ces deux sources, pas d'une lecture complète du crate — à
  compléter si un doute surgit dessus.

---

## 🔴 Décision/donnée encore nécessaire

1. **Table complète `capacité → marker(s) → bit`.** Seules `disclosure`
   (bit 1) et `map` (bit 2) sont confirmées. Restent à fournir, depuis le
   ledger existant du handoff initial — **pas à deviner** :
   - `image-focus`
   - `media-player`
   - `line-mark`
   - toute autre capacité listée en §4.1 de v1 non reprise ici.
2. **Forme HTML exacte de deux marqueurs**, à vérifier sur les templates/JS
   réels, pas à supposer :
   - `add-line-marks` : token `class`, ou attribut/autre forme ?
   - `figure-image-focus` : token nu séparé des variantes CSS
     (`class="figure-image-focus figure-image-focus-thumbnail-alignleft"`),
     ou concaténé sans le token nu ?
3. **Transport de `entry`** vers `AssetManifest` pour les capacités — jamais
   formellement reconfirmé cette session (distinct de `markers`/`activation`,
   qui eux ne transitent pas par le manifeste puisque `compute_js_deps` est
   écrit à la main, cf. 🟢 provisionnement SQL).
4. **Mécanisme de résolution `activation` + URL hashée de `entry` au
   rendu de page** — comment la paire `(import, appel)` évoquée dans le
   handoff initial est effectivement injectée dans le HTML servi à partir
   d'un bit `js_deps` positionné. Jamais détaillé dans cette session de
   conception SQL — à spécifier avant implémentation runtime.
5. **Outil de vérification croisée `theme.toml` ↔ SQL** (option B,
   explicitement différée : *« A pour cette session, éventuellement B plus
   tard »*) — non commencé, à ne pas entamer sans nouvel arbitrage.
6. **Migration ESM de `youtube.js`** — nom d'export à définir, non planifiée.
7. **Statut de `.progress`/`_base.js`** comme capacité `js_deps` — non
   tranché, explicitement hors périmètre pour l'instant.
8. **Forme du nouveau `FlatPageToken`** (hérité de v1, jamais traité dans
   cette session centrée sur `js_deps`/SQL — reporté ici par erreur de
   consolidation, réintégré). Concerne la représentation du point
   d'extension `MARIUS_MODULES` dans l'AST gelé de `fragment-forge` — à
   spécifier sans rouvrir l'invariant d'AST gelé lui-même (🟢, cf.
   `fragment-forge-lib.rs`).
9. **Emplacement concret de `<!-- MARIUS_MODULES -->`** dans `base.marius`
   (hérité de v1, même statut que le point 8 — jamais traité cette session).

---

## Reprise de session — instructions pour l'agent suivant

- Ne pas rouvrir les points 🟢 sans arbitrage explicite du concepteur.
- Commencer par les 🔴, dans l'ordre listé (le point 1 débloque la
  complétion de `compute_js_deps` ; les points 3-4 débloquent l'intégration
  runtime).
- Traiter les 🟡 comme des faits à vérifier au moment de les toucher, pas
  comme des inconnues bloquantes à résoudre en amont.
- Clause de méthode à reconduire : toute lecture de code JS/HTML existant
  sert à vérifier une conformité déjà décidée, jamais à en déduire une
  extension du contrat.
