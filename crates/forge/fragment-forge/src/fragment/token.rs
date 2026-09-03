// crates/forge/fragment-forge/src/fragment/token.rs

//! Phase 1.1 — Alphabet de l'AST Fragment : `FlatPageToken`.
//! Enum figé, matché de façon exhaustive par le validateur, le resolver et
//! le générateur AOT ; toute variante additionnelle est un breaking change
//! interne à documenter explicitement.

/// Token de l'AST d'un template `.marius`.
///
/// Le lifetime `'src` est lié à la durée de vie de la `String` source lue par
/// `std::fs::read_to_string` dans la fonction mère de `build.rs`.
/// L'AST ne sort jamais de cette portée : `'src` est localement borné,
/// jamais exposé à travers une frontière de thread ou de module.
///
/// Invariant : zéro allocation. Tous les champs texte sont des slices
/// pointant directement dans le buffer source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatPageToken<'src> {
    /// Segment HTML verbatim.
    Static(&'src str),

    /// Interpolation de champ : `{{ entity.field }}`.
    Field { entity: &'src str, field: &'src str },

    /// Bloc conditionnel booléen : `{% if entity.field %}`.
    IfBool { entity: &'src str, field: &'src str },

    /// Fermeture de bloc : `{% endif %}`.
    EndIf,

    /// Référence à un artefact préparé par `marius-assets` : `{% asset key %}`
    /// (spec `marius-assets-specification.md` §9). `key` est l'identifiant
    /// logique écrit tel quel par le développeur (`main.css`), jamais une
    /// URL — la résolution vers le chemin public versionné n'a jamais lieu
    /// ici. `fragment-forge` ne lit jamais le manifeste d'assets lui-même
    /// (aucun I/O dans ce module) : `resolve_and_measure` et
    /// `generate_aot_snippet` reçoivent chacun une closure de résolution
    /// injectée par `build.rs`, même patron que `StaticInclude`/
    /// `get_file_size` ci-dessous — à la différence que la closure ne
    /// renvoie jamais le contenu d'un fichier (inadapté : un asset est une
    /// URL, pas du texte à inliner), seulement une longueur (mesure) puis
    /// une chaîne résolue (émission).
    ///
    /// Contrairement à `StaticInclude`, ce token ne porte aucun champ
    /// provisoire à patcher en place : `resolve_and_measure` accumule
    /// directement la longueur résolue dans `TemplateMetrics`, sans jamais
    /// muter l'AST pour cette variante.
    AssetRef(&'src str),

    /// `{% script %}` — ouvre une région de capture opaque pour le
    /// hoisting des `<script>` (session dédiée). Symétrique à `IfBool` par
    /// la forme (marqueur de bloc, validé par `validate_ast`), mais
    /// orthogonal par le fond : `IfBool` gate un rendu RUNTIME (dépend de
    /// la ligne), `ScriptStart`/`ScriptEnd` délimitent une région connue
    /// intégralement à la COMPILATION — d'où deux états de FSM indépendants
    /// dans `validate_ast`, jamais un seul état partagé.
    ///
    /// Le contenu entre `ScriptStart` et `ScriptEnd` (typiquement un tag
    /// `<script>` complet écrit par le développeur, avec les attributs de
    /// SON choix — `defer`, `id`, `integrity`... — ce Parser n'a et n'aura
    /// jamais de connaissance de la grammaire HTML `<script>` elle-même)
    /// est capturé verbatim par `hoist_and_dedupe_scripts`. Ces deux
    /// marqueurs eux-mêmes n'émettent jamais rien dans `generate_aot_snippet`
    /// (No-Op pur) : c'est `build.rs` qui décide, selon que la cible de
    /// compilation est une Page (layout avec `<head>`) ou un Fragment
    /// isolé, d'appeler ou non la passe de hoisting en amont — cette
    /// distinction ne vit jamais dans ce crate.
    ScriptStart,
    /// `{% endscript %}` — ferme la région ouverte par `ScriptStart`.
    ScriptEnd,

    /// Inclusion statique résolue au build-time : `{% include path %}`.
    ///
    /// `len` : longueur en octets du fichier inclus, connue à la compilation
    /// (via `std::fs::metadata`). Composante directe de `PAGE_STATIC_CAP`.
    StaticInclude {
        original_path: &'src str,
        rel_from_manifest: &'src str,
        len: usize,
    },

    /// Point d'extension textuel post-abaissement : `<!-- MARIUS_MODULES -->`
    /// dans `base.marius`, position sœur de `ScriptStart`/`ScriptEnd`
    /// (HANDOFF-js-deps-capacites-frontend-v2.md, § Lowering AOT de
    /// `js_deps`). Jamais produit par le scanner/parser — `{% %}`/`{{ }}`
    /// restent les seules syntaxes actives de ce crate. Injecté directement
    /// dans le flux de tokens par `build.rs`, après `lower`, par recherche
    /// de sous-chaîne dans un `Static` : même mécanisme que
    /// `SCRIPTS_PLACEHOLDER`/`splice_hoisted_scripts`, jamais un nouveau
    /// chemin de parsing.
    ///
    /// Ne porte aucune donnée propre : `build.rs` calcule intégralement, en
    /// amont, la vue de compilation `bit → (URL, activation)` (lecture de
    /// `theme.toml`, `scripts_registry.lock`, `AssetManifest`) et la fournit
    /// à `resolve_and_measure` sous forme d'une longueur (mesure), puis à
    /// `generate_aot_snippet`/`generate_segmented_snippet` sous forme d'une
    /// chaîne de code Rust déjà assemblée (émission) — `fragment-forge` ne
    /// connaît et ne doit connaître aucune des trois sources.
    ///
    /// Contexte de lowering dépendant de l'appelant — propriété du
    /// CONTEXTE, jamais de ce token lui-même : `resolve_page_template`
    /// (Mode Page, `record` réel) fournit la vue calculée ; `resolve_static_page`
    /// (`STATIC_PAGES`, aucun `record`) fournit systématiquement 0 octet /
    /// chaîne vide — un ensemble de capacités par définition vide pour une
    /// page sans état éditorial, jamais un cas d'erreur ni un no-op
    /// accidentel : c'est le comportement normal du lowering dans ce
    /// pipeline.
    ModulesPlaceholder,
}

#[cfg(test)]
mod tests_phase_1_1 {
    use super::FlatPageToken;

    /// Jalon Vert Phase 1.1 — compilation sans annotation de lifetime explicite.
    ///
    /// Le compilateur infère `FlatPageToken<'_>` depuis la durée de vie de `src`.
    /// Aucune annotation `<'static>` ni `<'_>` n'est requise au site de construction.
    #[test]
    fn static_variant_infers_lifetime() {
        let src: &str = "hello";
        let token = FlatPageToken::Static(src);
        match token {
            FlatPageToken::Static(s) => assert_eq!(s, "hello"),
            _ => unreachable!(),
        }
    }

    /// Vérifie que `Copy` est disponible sur tous les variants.
    ///
    /// Preuve : `tokens[0]` est réaffecté deux fois sans move.
    /// Si `Copy` manquait (champ non-Copy), ce test ne compilerait pas.
    #[test]
    fn all_variants_are_copy() {
        let tokens: [FlatPageToken<'_>; 6] = [
            FlatPageToken::Static("content"),
            FlatPageToken::Field {
                entity: "user",
                field: "name",
            },
            FlatPageToken::IfBool {
                entity: "user",
                field: "active",
            },
            FlatPageToken::EndIf,
            FlatPageToken::StaticInclude {
                original_path: "templates/header.html",
                rel_from_manifest: "../templates/header.html",
                len: 42,
            },
            FlatPageToken::ModulesPlaceholder,
        ];

        let _a = tokens[0]; // premier move apparent
        let _b = tokens[0]; // second : compile ssi Copy est implémenté
    }
}
