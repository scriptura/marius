//! Hoisting + déduplication des blocs `{% script %}...{% endscript %}`.
//! Passe de compilation unique (`build.rs`), jamais exécutée par requête ;
//! propriété de la CIBLE de compilation (Page vs Fragment isolé), jamais
//! de l'AST lui-même — voir doc de tête ci-dessous.

use crate::fragment::token::FlatPageToken;

// =============================================================================
// Hoisting + déduplication des `{% script %}...{% endscript %}` — passe de
// capture de bloc (révision de session : remplace l'ancienne approche par
// clé `AssetRef(*.js)` seule).
//
// Rappel de la correction architecturale déjà actée (inchangée) : cette
// passe tourne UNE FOIS à la compilation (`build.rs`), jamais par requête —
// aucun `HashSet` ne survit au-delà de la génération du fichier `.rs`
// source.
//
// Changement de grammaire (session dédiée) : le développeur écrit
// désormais son tag `<script>` complet, avec les attributs de SON choix
// (`defer`, `id`, `integrity`...), entouré de `{% script %}`/
// `{% endscript %}` — ce crate n'a et n'aura jamais de connaissance de la
// grammaire HTML `<script>` elle-même (pas de couplage présentation/
// compilateur). La région entière est capturée verbatim comme une
// sous-séquence opaque de `FlatPageToken`, jamais reconstruite depuis du
// texte.
//
// Modèle physique du moteur (précisé en session) : un fichier `.marius`
// peut être compilé comme composant d'une Page complète (layout avec
// `<head>`) ou comme Partial autonome (Fragment isolé, résolu directement
// par `resolve_template`, sans layout). Le hoisting n'est donc PAS une
// propriété de l'AST — c'est une propriété de la CIBLE de compilation :
//   - Cible Page   : `build.rs` appelle cette passe, extrait et dédup-
//     lique les blocs, les réinjecte au marqueur `<head>`.
//   - Cible Fragment isolé : `build.rs` n'appelle jamais cette passe.
//     `ScriptStart`/`ScriptEnd` traversent `generate_aot_snippet` comme de
//     purs No-Op (voir leurs bras dédiés plus haut) — le contenu capturé
//     reste alors inline, à sa position d'origine, exactement comme si les
//     deux marqueurs n'existaient pas.
// Cette distinction ne vit jamais dans ce crate : `hoist_and_dedupe_scripts`
// n'est JAMAIS appelée pour une cible Fragment isolé, décision prise
// entièrement par l'orchestrateur.
// =============================================================================

/// Erreur de la passe de hoisting/déduplication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoistError {
    /// Un bloc `{% script %}...{% endscript %}` trouvé À L'INTÉRIEUR d'un
    /// bloc `{% if %}...{% endif %}` ouvert — non supporté par cette passe
    /// (restriction explicitement validée en session : "l'arbre des
    /// dépendances doit rester prévisible à la compilation"). Son
    /// inclusion dépendrait d'une donnée RUNTIME (la ligne effectivement
    /// rendue), alors que cette passe s'exécute UNE FOIS pour tout le
    /// template, indépendamment des données — le hisser quand même le
    /// rendrait inconditionnel, un vrai bug de correction, pas une
    /// simplification acceptable.
    ConditionalScript,
    /// `{% endscript %}` sans `{% script %}` ouvert correspondant, ou fin
    /// de flux avec un bloc encore ouvert. Ne devrait structurellement
    /// jamais se produire si `validate_ast` a déjà validé le flux (sa
    /// propre FSM garantit cet équilibre) — cette fonction ne SUPPOSE pas
    /// cette précondition pour autant : elle reste défensive plutôt que de
    /// paniquer si elle est un jour appelée directement sur un flux non
    /// validé (tests compris, voir plus bas).
    UnbalancedScriptBlock,
}

impl std::fmt::Display for HoistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HoistError::ConditionalScript => write!(
                f,
                "hoisting : un bloc {{% script %}}...{{% endscript %}} à l'intérieur d'un \
                 bloc {{% if %}} n'est pas supporté (portée conditionnelle non résolvable \
                 à la compilation) — voir doc de HoistError::ConditionalScript"
            ),
            HoistError::UnbalancedScriptBlock => write!(
                f,
                "hoisting : bloc {{% script %}}/{{% endscript %}} déséquilibré (devrait \
                 avoir été détecté par validate_ast en amont)"
            ),
        }
    }
}

impl std::error::Error for HoistError {}

/// Extrait, déduplique et retire du flux les blocs `{% script %}...
/// {% endscript %}` INCONDITIONNELS d'une page déjà linéarisée
/// (post-`link`/`lower`, post-`validate_ast`). La duplication vient de
/// `{% include %}` : chaque occurrence d'un fragment copie l'intégralité
/// de ses tokens dans le flux de la page, y compris ses éventuels blocs
/// `script` — sans cette passe, un fragment inclus trois fois produit
/// trois `<script>` identiques.
///
/// Déduplication par ÉGALITÉ DE CONTENU (comparaison structurelle de la
/// sous-séquence de tokens capturée), pas par une clé synthétique — deux
/// blocs capturés IDENTIQUES (mêmes tokens, dans le même ordre) sont
/// nécessairement la même répétition accidentelle d'un fragment inclus
/// plusieurs fois ; deux blocs qui DIFFÈRENT (attributs différents sur le
/// même asset, par exemple) sont considérés distincts et tous deux
/// conservés — jamais de suppression silencieuse d'un attribut que le
/// développeur a écrit explicitement sur l'une des deux occurrences.
/// Comparaison en O(n²) sur le nombre de blocs DISTINCTS déjà vus (une
/// poignée par page en pratique) : la structure de données la plus simple
/// suffit, pas la peine d'imposer `Hash` à `FlatPageToken` pour un
/// `HashSet` dont le gain serait invisible à cette échelle.
///
/// Retourne `(flux_sans_les_blocs_script, blocs_uniques_dans_l'ordre_de_
/// première_apparition)` — chaque bloc est une sous-séquence de tokens
/// verbatim (le tag `<script>` complet écrit par le développeur), prête à
/// être réinjectée telle quelle par `splice_hoisted_scripts`.
pub fn hoist_and_dedupe_scripts<'src>(
    tokens: Vec<FlatPageToken<'src>>,
) -> Result<(Vec<FlatPageToken<'src>>, Vec<Vec<FlatPageToken<'src>>>), HoistError> {
    let mut output = Vec::with_capacity(tokens.len());
    let mut captured_blocks: Vec<Vec<FlatPageToken<'src>>> = Vec::new();
    let mut if_depth: u32 = 0;

    let mut iter = tokens.into_iter();
    while let Some(token) = iter.next() {
        match token {
            FlatPageToken::IfBool { .. } => {
                if_depth += 1;
                output.push(token);
            }
            FlatPageToken::EndIf => {
                if_depth = if_depth.saturating_sub(1);
                output.push(token);
            }
            FlatPageToken::ScriptStart => {
                if if_depth > 0 {
                    return Err(HoistError::ConditionalScript);
                }

                let mut block: Vec<FlatPageToken<'src>> = Vec::new();
                loop {
                    match iter.next() {
                        Some(FlatPageToken::ScriptEnd) => break,
                        Some(inner) => block.push(inner),
                        None => return Err(HoistError::UnbalancedScriptBlock),
                    }
                }

                // Extraction : ni le marqueur, ni son contenu, ni un
                // doublon détecté ne rejoignent jamais `output` — "zéro
                // trace locale" (mission précédente §1), à l'échelle du
                // bloc entier cette fois, pas d'un seul token `AssetRef`.
                if !captured_blocks.iter().any(|seen| seen == &block) {
                    captured_blocks.push(block);
                }
            }
            FlatPageToken::ScriptEnd => {
                // Rencontré hors capture : aucun ScriptStart ouvert à ce
                // niveau n'a consommé ce token via la boucle interne
                // ci-dessus.
                return Err(HoistError::UnbalancedScriptBlock);
            }
            other => output.push(other),
        }
    }

    Ok((output, captured_blocks))
}

/// Réinjecte les blocs de scripts hissés à une position déjà déterminée
/// par l'appelant (`build.rs` — voir note d'intégration en tête de
/// section : cette fonction reste délibérément agnostique de la façon
/// dont cette position est repérée, pour ne pas exiger de modification de
/// l'AST gelé de ce crate).
///
/// Simple concaténation, dans l'ordre reçu (déjà déterministe : ordre de
/// première apparition, si issu de `hoist_and_dedupe_scripts`) — aucune
/// balise n'est SYNTHÉTISÉE ici : chaque bloc capturé contient déjà,
/// verbatim, le tag `<script>` complet écrit par le développeur
/// (attributs compris). Cette fonction assemble des blocs déjà résolus,
/// elle ne génère jamais de HTML elle-même.
///
/// `at_index` : position dans `tokens` où insérer le bloc assemblé — le
/// token initialement à cette position est décalé après, jamais écrasé.
pub fn splice_hoisted_scripts<'src>(
    mut tokens: Vec<FlatPageToken<'src>>,
    hoisted_blocks: &[Vec<FlatPageToken<'src>>],
    at_index: usize,
) -> Vec<FlatPageToken<'src>> {
    if hoisted_blocks.is_empty() {
        return tokens; // rien à hisser : flux inchangé.
    }

    let mut block: Vec<FlatPageToken<'src>> = Vec::new();
    for captured in hoisted_blocks {
        block.extend(captured.iter().copied());
    }

    let insert_at = at_index.min(tokens.len());
    let tail = tokens.split_off(insert_at);
    tokens.extend(block);
    tokens.extend(tail);
    tokens
}

// =============================================================================
// Tests — Hoisting + déduplication des scripts (capture de bloc).
// =============================================================================

#[cfg(test)]
mod tests_hoist_scripts {
    use super::{FlatPageToken, HoistError, hoist_and_dedupe_scripts, splice_hoisted_scripts};

    /// Reproduit exactement l'exemple `core.marius` de la mission : le
    /// MÊME bloc `<script src="{% asset map.js %}" type="module">
    /// </script>` écrit deux fois d'affilée.
    #[test]
    fn hoist_removes_block_entirely_and_dedupes_identical_repeats() {
        let tokens = vec![
            FlatPageToken::Static("<p>1</p>"),
            FlatPageToken::ScriptStart,
            FlatPageToken::Static("<script src=\""),
            FlatPageToken::AssetRef("map.js"),
            FlatPageToken::Static("\" type=\"module\"></script>"),
            FlatPageToken::ScriptEnd,
            FlatPageToken::ScriptStart,
            FlatPageToken::Static("<script src=\""),
            FlatPageToken::AssetRef("map.js"),
            FlatPageToken::Static("\" type=\"module\"></script>"),
            FlatPageToken::ScriptEnd,
            FlatPageToken::Static("<p>2</p>"),
        ];

        let (output, blocks) = hoist_and_dedupe_scripts(tokens).unwrap();

        // Une seule occurrence malgré deux blocs sources identiques.
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0],
            vec![
                FlatPageToken::Static("<script src=\""),
                FlatPageToken::AssetRef("map.js"),
                FlatPageToken::Static("\" type=\"module\"></script>"),
            ]
        );
        // Zéro trace locale : ni marqueurs, ni contenu, ni doublon.
        assert_eq!(
            output,
            vec![
                FlatPageToken::Static("<p>1</p>"),
                FlatPageToken::Static("<p>2</p>"),
            ]
        );
    }

    /// Deux blocs référençant le MÊME asset mais avec des attributs
    /// DIFFÉRENTS (ex. un `id` sur l'un, pas sur l'autre) ne doivent
    /// jamais fusionner — l'un des deux attributs serait silencieusement
    /// perdu si la déduplication se faisait par clé d'asset plutôt que par
    /// égalité de contenu complet.
    #[test]
    fn hoist_keeps_distinct_blocks_on_same_asset_as_separate_entries() {
        let tokens = vec![
            FlatPageToken::ScriptStart,
            FlatPageToken::Static("<script src=\""),
            FlatPageToken::AssetRef("map.js"),
            FlatPageToken::Static("\" type=\"module\"></script>"),
            FlatPageToken::ScriptEnd,
            FlatPageToken::ScriptStart,
            FlatPageToken::Static("<script src=\""),
            FlatPageToken::AssetRef("map.js"),
            FlatPageToken::Static("\" type=\"module\" id=\"map-loader\"></script>"),
            FlatPageToken::ScriptEnd,
        ];

        let (_, blocks) = hoist_and_dedupe_scripts(tokens).unwrap();

        assert_eq!(
            blocks.len(),
            2,
            "deux tags distincts, aucun attribut ne doit disparaître"
        );
    }

    #[test]
    fn hoist_preserves_first_occurrence_order_across_distinct_blocks() {
        let tokens = vec![
            FlatPageToken::ScriptStart,
            FlatPageToken::AssetRef("b.js"),
            FlatPageToken::ScriptEnd,
            FlatPageToken::ScriptStart,
            FlatPageToken::AssetRef("a.js"),
            FlatPageToken::ScriptEnd,
        ];

        let (_, blocks) = hoist_and_dedupe_scripts(tokens).unwrap();

        assert_eq!(
            blocks,
            vec![
                vec![FlatPageToken::AssetRef("b.js")],
                vec![FlatPageToken::AssetRef("a.js")]
            ]
        );
    }

    #[test]
    fn hoist_unconditional_script_outside_any_if_is_captured() {
        let tokens = vec![
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_published",
            },
            FlatPageToken::Static("<p>x</p>"),
            FlatPageToken::EndIf,
            FlatPageToken::ScriptStart,
            FlatPageToken::AssetRef("nav.js"),
            FlatPageToken::ScriptEnd,
        ];

        let (output, blocks) = hoist_and_dedupe_scripts(tokens).unwrap();

        assert_eq!(blocks, vec![vec![FlatPageToken::AssetRef("nav.js")]]);
        assert_eq!(
            output,
            vec![
                FlatPageToken::IfBool {
                    entity: "record",
                    field: "is_published"
                },
                FlatPageToken::Static("<p>x</p>"),
                FlatPageToken::EndIf,
            ]
        );
    }

    /// Restriction explicitement validée en session : un bloc `script` à
    /// l'intérieur d'un `if` doit échouer, jamais être hissé de façon
    /// inconditionnelle.
    #[test]
    fn hoist_conditional_script_is_a_hard_error() {
        let tokens = vec![
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_published",
            },
            FlatPageToken::ScriptStart,
            FlatPageToken::AssetRef("extra.js"),
            FlatPageToken::ScriptEnd,
            FlatPageToken::EndIf,
        ];

        assert_eq!(
            hoist_and_dedupe_scripts(tokens),
            Err(HoistError::ConditionalScript)
        );
    }

    #[test]
    fn hoist_unterminated_script_block_is_an_error() {
        let tokens = vec![FlatPageToken::ScriptStart, FlatPageToken::AssetRef("x.js")];
        assert_eq!(
            hoist_and_dedupe_scripts(tokens),
            Err(HoistError::UnbalancedScriptBlock)
        );
    }

    #[test]
    fn hoist_end_script_without_start_is_an_error() {
        let tokens = vec![FlatPageToken::ScriptEnd];
        assert_eq!(
            hoist_and_dedupe_scripts(tokens),
            Err(HoistError::UnbalancedScriptBlock)
        );
    }

    #[test]
    fn splice_inserts_hoisted_blocks_verbatim_in_order() {
        let tokens = vec![
            FlatPageToken::Static("<head>"),
            FlatPageToken::Static("</head>"),
        ];
        let blocks = vec![
            vec![FlatPageToken::AssetRef("main.js")],
            vec![FlatPageToken::AssetRef("more.js")],
        ];

        let result = splice_hoisted_scripts(tokens, &blocks, 1);

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("<head>"),
                FlatPageToken::AssetRef("main.js"),
                FlatPageToken::AssetRef("more.js"),
                FlatPageToken::Static("</head>"),
            ]
        );
    }

    #[test]
    fn splice_with_no_hoisted_blocks_leaves_stream_unchanged() {
        let tokens = vec![FlatPageToken::Static("<head></head>")];
        let result = splice_hoisted_scripts(tokens.clone(), &[], 0);
        assert_eq!(result, tokens);
    }

    /// Bout-en-bout : hoist puis splice reproduit le scénario complet de
    /// la mission — fragment de nav inclus deux fois (contenu identique)
    /// et fragment "map" inclus deux fois également, un seul exemplaire
    /// de chacun dans le flux final, à l'emplacement du marqueur.
    #[test]
    fn hoist_then_splice_end_to_end() {
        let nav_block = vec![
            FlatPageToken::ScriptStart,
            FlatPageToken::Static("<script src=\""),
            FlatPageToken::AssetRef("nav.js"),
            FlatPageToken::Static("\" type=\"module\"></script>"),
            FlatPageToken::ScriptEnd,
        ];

        let mut tokens = vec![
            FlatPageToken::Static("<head>"),
            FlatPageToken::Static("</head>"),
        ];
        tokens.push(FlatPageToken::Static("<body>"));
        tokens.extend(nav_block.clone()); // 1ère inclusion du fragment de nav
        tokens.push(FlatPageToken::Static("<hr>"));
        tokens.extend(nav_block); // 2ème inclusion, contenu identique
        tokens.push(FlatPageToken::Static("</body>"));

        let (mut output, blocks) = hoist_and_dedupe_scripts(tokens).unwrap();
        assert_eq!(blocks.len(), 1); // dédupliqué malgré deux inclusions

        let head_close = output
            .iter()
            .position(|t| matches!(t, FlatPageToken::Static(s) if *s == "</head>"))
            .unwrap();
        output = splice_hoisted_scripts(output, &blocks, head_close);

        assert_eq!(
            output,
            vec![
                FlatPageToken::Static("<head>"),
                FlatPageToken::Static("<script src=\""),
                FlatPageToken::AssetRef("nav.js"),
                FlatPageToken::Static("\" type=\"module\"></script>"),
                FlatPageToken::Static("</head>"),
                FlatPageToken::Static("<body>"),
                FlatPageToken::Static("<hr>"),
                FlatPageToken::Static("</body>"),
            ]
        );
    }
}
