// crates/assets/src/styles.rs

//! Pipeline `[styles]`.
//!
//! Pipeline de transformation AOT des feuilles de styles : bundling, validation stricte
//! des ressources typographiques, et minification via `lightningcss`.
//!
//! ## Modèle de Compilation & Hachage
//!
//! - **Empreinte de sortie :** Le calcul du hachage s'effectue sur le buffer de sortie transformé
//!   (le payload qui sera effectivement servi au client), jamais sur les fichiers sources isolés.
//! - **Aplatissement spatial :** Les sous-répertoires de *staging* (ex: `development/`) sont
//!   délibérément absorbés. Le *data layout* de sortie est strictement plat sous `build_root/styles/`,
//!   éliminant l'indirection des résolutions de chemins au runtime.
//!
//! ## Dialecte Étendu (Zero-Sass)
//!
//! Le pipeline implémente un visiteur d'AST `lightningcss` natif pour résoudre un dialecte minimaliste
//! (variables `$vars`, boucles `@for`) sans subir l'empreinte mémoire ni l'opacité d'un préprocesseur
//! externe lourd. La résolution des directives `url()` est interceptée et couplée au registre d'assets.

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lightningcss::bundler::{Bundler, ResolveResult, SourceProvider};
use lightningcss::rules::CssRule;
use lightningcss::stylesheet::{MinifyOptions, ParserOptions, PrinterOptions};
use lightningcss::values::url::Url;
use lightningcss::visit_types;
use lightningcss::visitor::{Visit, VisitTypes, Visitor};

use crate::manifest::{
    AssetEntry, AssetUrlRegistry, CanonicalAssetId, hash_content, join_slash, mime_for_extension,
};
use crate::resolve::{ReferenceOrigin, resolve_asset_reference};

/// Erreur de résolution lors de l'évaluation d'une directive `url()` CSS (Spec §10.1, Roadmap §1.8).
///
/// ## Invariant structurel (Échec dur)
///
/// Se déclenche de manière déterministe si la ressource référencée est introuvable dans
/// l'`AssetUrlRegistry`.
///
/// Le pipeline interdit tout repli silencieux (*passthrough*) vers une URL non versionnée.
/// La résolution du graphe de dépendances statiques doit être garantie AOT pour s'assurer
/// que chaque asset pointe vers un fragment mémoire validé, empêchant toute erreur réseau
/// (HTTP 404) au runtime.
#[derive(Debug)]
struct CssUrlResolutionError(String);

impl fmt::Display for CssUrlResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AssetNotFound (CSS url(), spec §10.1 / Roadmap §1.8) : {}",
            self.0
        )
    }
}

impl std::error::Error for CssUrlResolutionError {}

/// Extrait `source_index` de n'importe quelle variante `CssRule` portant
/// un `loc` — vérifié exhaustivement contre `lightningcss = "=1.0.0-alpha.71"`
/// (expérience réelle, SPEC-canonical-asset-identity.md §7) : toutes les
/// variantes en portent un sauf `Ignored` (placeholder vide) et `Custom`
/// (générique, dépend d'un type fourni par l'appelant, non utilisé ici).
/// `match` volontairement non-`_` sur le reste : l'ajout d'une future
/// variante par une mise à jour de `lightningcss` fait échouer la
/// compilation plutôt que de silencieusement perdre la provenance pour ce
/// nouveau cas.
fn extract_source_index(rule: &CssRule) -> Option<u32> {
    match rule {
        CssRule::Media(r) => Some(r.loc.source_index),
        CssRule::Import(r) => Some(r.loc.source_index),
        CssRule::Style(r) => Some(r.loc.source_index),
        CssRule::Keyframes(r) => Some(r.loc.source_index),
        CssRule::FontFace(r) => Some(r.loc.source_index),
        CssRule::FontPaletteValues(r) => Some(r.loc.source_index),
        CssRule::FontFeatureValues(r) => Some(r.loc.source_index),
        CssRule::Page(r) => Some(r.loc.source_index),
        CssRule::Supports(r) => Some(r.loc.source_index),
        CssRule::CounterStyle(r) => Some(r.loc.source_index),
        CssRule::Namespace(r) => Some(r.loc.source_index),
        CssRule::MozDocument(r) => Some(r.loc.source_index),
        CssRule::Nesting(r) => Some(r.loc.source_index),
        CssRule::NestedDeclarations(r) => Some(r.loc.source_index),
        CssRule::Viewport(r) => Some(r.loc.source_index),
        CssRule::CustomMedia(r) => Some(r.loc.source_index),
        CssRule::LayerStatement(r) => Some(r.loc.source_index),
        CssRule::LayerBlock(r) => Some(r.loc.source_index),
        CssRule::Property(r) => Some(r.loc.source_index),
        CssRule::Container(r) => Some(r.loc.source_index),
        CssRule::Scope(r) => Some(r.loc.source_index),
        CssRule::StartingStyle(r) => Some(r.loc.source_index),
        CssRule::ViewTransition(r) => Some(r.loc.source_index),
        CssRule::Unknown(r) => Some(r.loc.source_index),
        CssRule::Ignored | CssRule::Custom(_) => None,
    }
}

/// Visiteur AST — résout TOUT `url()` du document contre
/// `AssetUrlRegistry` (Phase 5 : Roadmap §1.8 tranchée — `background-image`,
/// `mask`, `cursor`, etc., pas seulement `@font-face`). La validation dure
/// (échec si absent du registre) s'applique désormais uniformément, pas
/// seulement aux polices.
///
/// SPEC-canonical-asset-identity.md §7 — provenance correcte, y compris à
/// travers `@import` imbriqués et règles imbriquées (`@media`, etc.) :
/// vérifié expérimentalement contre le comportement réel de
/// `lightningcss::Bundler` (jamais supposé). `Bundler` ne rebase JAMAIS un
/// `url()` relatif à l'inlining d'un `@import` — le littéral reste
/// strictement celui écrit dans le fichier source réel. La provenance
/// correcte est récupérée via `Url.loc` — non, ce champ ne porte que
/// `line`/`column` — mais via le `loc.source_index` de la RÈGLE
/// englobante, capturé à l'entrée de `visit_rule`, avant de descendre
/// dans ses déclarations (ordre de traversée en profondeur garanti par
/// `Visit`).
struct CssUrlVisitor<'a> {
    registry: &'a AssetUrlRegistry,
    /// `theme_dir` — nécessaire pour transformer un chemin de
    /// `stylesheet.sources[i]` (espace du système de fichiers, tel que
    /// transmis à `Bundler`/`SourceProvider`) en `CanonicalAssetId`
    /// (relatif à la racine du thème). Les deux espaces coïncident
    /// aujourd'hui par construction (`entry_path = theme_dir.join(...)`,
    /// `MvarProvider::resolve` dérive tout le reste par
    /// `with_file_name()`), mais rien ne garantit cette forme sans
    /// vérification explicite ici — d'où le `strip_prefix` plutôt qu'une
    /// supposition silencieuse.
    theme_dir: &'a Path,
    /// `stylesheet.sources` — index → chemin réel du fichier source,
    /// peuplé par `Bundler` lui-même (un par fichier du graphe
    /// `@import`, entrée comprise).
    sources: &'a [String],
    /// État de traversée — dernière règle porteuse d'un `loc` rencontrée.
    /// Ne redescend jamais à `None` en sortant d'une règle : l'ordre de
    /// traversée en profondeur garantit qu'à chaque `visit_url`, cet état
    /// reflète toujours la bonne règle (aucun `url()` ne peut être visité
    /// avant qu'au moins une règle englobante n'ait déjà été visitée —
    /// la grammaire CSS ne permet aucune déclaration hors de toute règle).
    current_source_index: Option<u32>,
}

impl<'i> Visitor<'i> for CssUrlVisitor<'_> {
    type Error = CssUrlResolutionError;

    fn visit_types(&self) -> VisitTypes {
        visit_types!(URLS | RULES)
    }

    fn visit_rule(&mut self, rule: &mut CssRule<'i>) -> Result<(), Self::Error> {
        if let Some(idx) = extract_source_index(rule) {
            self.current_source_index = Some(idx);
        }
        rule.visit_children(self)
    }

    fn visit_url(&mut self, url: &mut Url<'i>) -> Result<(), Self::Error> {
        let idx = self.current_source_index.expect(
            "current_source_index doit déjà être posé : aucune déclaration CSS \
             (donc aucun url()) n'existe hors de toute règle, la grammaire CSS \
             l'interdit structurellement — si ce panic se déclenche, c'est que \
             cette garantie a été violée ailleurs, pas un cas à absorber ici",
        );
        let source_path = &self.sources[idx as usize];
        let rel = Path::new(source_path)
            .strip_prefix(self.theme_dir)
            .unwrap_or(Path::new(source_path));
        let origin_id = CanonicalAssetId::from_theme_relative_path(rel);

        match resolve_asset_reference(
            url.url.as_ref(),
            ReferenceOrigin::RelativeToFile(&origin_id),
            self.registry,
        ) {
            Ok(Some(resolved)) => {
                url.url = resolved.into();
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(message) => Err(CssUrlResolutionError(message)),
        }
    }
}

// =============================================================================
// Phase 3 — Résolution AOT des `$variables` (dialecte Sass-like du thème).
//
// Piège identifié : `lightningcss` est un parseur W3C strict, il ne peut
// jamais voir un token `$nom`. Toute pré-passe doit donc s'exécuter
// AVANT que quoi que ce soit ne soit tendu à `Bundler`/`StyleSheet::parse`.
//
// Écart écarté : substituer à l'intérieur d'un `SourceProvider::read()`
// pris isolément, fichier par fichier. Ordre réel d'appel du `Bundler` :
// il lit d'abord le texte BRUT du fichier d'entrée en entier, PUIS le
// parse, et ne découvre (donc ne lit) un `@import` qu'à ce moment-là —
// après coup. Si `$brand-primary` est déclaré dans un partial importé
// mais utilisé dans le fichier d'entrée, le registre serait encore vide
// au moment de traiter l'entrée : substitution manquée, pas d'erreur
// franche, corruption silencieuse du CSS émis. C'est exactement le piège
// signalé par l'auteur du projet.
//
// Conséquence architecturale : deux passes strictement séparées, jamais
// fusionnées dans un seul appel — même discipline que la séparation
// extraction-d'usage / substitution actée en Roadmap §2.1 pour le futur
// tree-shaking (éviter un faux cycle en confondant deux passes qui n'ont
// pas la même dépendance de données) :
//
//   Passe A — walk_variable_graph  (lecture seule, texte brut)
//     Parcourt le graphe `@import` par un scan textuel minimal — PAS par
//     `lightningcss` (qui crasherait). Construit le VariableRegistry
//     complet pour TOUT le graphe avant que quiconque ne songe à
//     substituer quoi que ce soit. Ne connaît aucune sémantique CSS
//     (`layer(...)`, media, supports) — seul le chemin importé l'intéresse.
//
//   Passe B — MvarProvider (SourceProvider custom, remplace FileProvider)
//     Une fois le registre figé, `Bundler` s'exécute normalement pour la
//     sémantique réelle (`@import`, `layer(...)`, media/supports — cf.
//     Handoff §1, non ré-implémentée ici). Le seul point d'interception
//     est `read()` : chaque fichier, qu'il soit l'entrée ou n'importe
//     quel import résolu par `Bundler`, passe par la substitution avant
//     que son texte n'atteigne le parseur. Un seul point d'ancrage
//     garantit la couverture totale du graphe sans dupliquer la logique
//     de résolution d'imports de `Bundler` lui-même.
// =============================================================================

/// Registre des variables `$nom -> valeur`, construit par la Passe A et
/// figé (lecture seule) pendant toute la Passe B. Pas de `RefCell`/mutation
/// partagée : le cycle de vie séquentiel (registre complet AVANT premher
/// `read()`) rend toute synchronisation runtime inutile.
type VariableRegistry = HashMap<String, String>;

#[derive(Debug)]
enum MvarError {
    Io(std::io::Error),
    /// `$nom` rencontré à la substitution mais absent du registre — échec
    /// dur, même politique que `CssUrlResolutionError` : pas de passthrough
    /// silencieux d'un token non résolu vers le CSS final.
    ///
    /// `suggestion` est calculée UNE SEULE FOIS, au point de construction
    /// de l'erreur (`substitute_line`, qui a déjà `&VariableRegistry` sous
    /// la main) — pas au moment de l'affichage. Ce n'est pas un détail
    /// cosmétique : l'erreur transporte déjà tout ce dont `Display` a
    /// besoin, sans lui redonner accès au registre.
    UndefinedVariable {
        name: String,
        file: PathBuf,
        suggestion: Option<String>,
    },
    /// Grammaire `@for` malformée (borne manquante, accolade non fermée,
    /// pas nul, etc.) — voir `ForLoopError` plus bas dans ce fichier.
    ForLoop(ForLoopError),
    /// Chaîne ou commentaire CSS non fermé — voir `CssCommentError`,
    /// `strip_css_comments` plus bas. Détecté avant même la recherche de
    /// `$variables`/`@for`, donc toujours la première erreur possible sur
    /// un fichier donné.
    Comment(CssCommentError),
}

impl fmt::Display for MvarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MvarError::Io(e) => write!(f, "styles (variables) : lecture impossible : {e}"),
            MvarError::UndefinedVariable {
                name,
                file,
                suggestion,
            } => {
                write!(
                    f,
                    "styles (variables) : ${name} utilisée mais jamais déclarée (fichier {})",
                    file.display()
                )?;
                match suggestion {
                    Some(hint) => write!(f, " — {hint}"),
                    None => write!(
                        f,
                        " — aucune variable proche dans le registre ; vérifiez l'orthographe \
                         et la présence de la déclaration `${name}: valeur;`."
                    ),
                }
            }
            MvarError::ForLoop(e) => write!(f, "{e}"),
            MvarError::Comment(e) => write!(f, "{e}"),
        }
    }
}

/// Suggestion pour un `$nom` non résolu — deux niveaux de confiance,
/// jamais mélangés dans le même message (une correspondance insensible à
/// la casse est quasi certaine, une correspondance approchée par distance
/// d'édition ne l'est pas, le message ne doit pas prétendre le contraire).
///
/// Priorité 1 — casse différente : le cas le plus probable en pratique
/// (l'auteur du projet a confirmé ce comportement lors de la session
/// précédente : `${name}` sensible à la casse est un choix assumé, pas un
/// bug — mais une faute de casse reste l'erreur la plus fréquente pour
/// autant, elle mérite un message qui la nomme explicitement plutôt qu'un
/// "vouliez-vous dire" générique.
///
/// Priorité 2 — faute de frappe : plus proche voisin par distance de
/// Levenshtein, borné à 2 pour éviter une suggestion trompeuse sur un nom
/// sans rapport réel (mieux vaut aucune suggestion qu'une mauvaise piste).
fn suggest_variable(name: &str, registry: &VariableRegistry) -> Option<String> {
    if let Some(exact_ci) = registry.keys().find(|k| k.eq_ignore_ascii_case(name)) {
        return Some(format!(
            "la casse ne correspond pas : le registre contient ${exact_ci}, pas ${name} \
             (la casse est sensible, comportement assumé)"
        ));
    }

    registry
        .keys()
        .map(|k| (k, levenshtein(name, k)))
        .filter(|(_, dist)| *dist <= 2)
        .min_by_key(|(_, dist)| *dist)
        .map(|(k, _)| format!("vouliez-vous dire ${k} ?"))
}

/// Distance de Levenshtein — classique, deux lignes de tableau roulées
/// (`prev`/`curr`) plutôt qu'une matrice complète : le registre de
/// $variables d'un thème compte au plus quelques dizaines d'entrées, la
/// complexité O(n·m) par comparaison est hors de propos ici, seule
/// l'empreinte mémoire (une matrice complète serait un gaspillage sans
/// contrepartie) justifie ce choix.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];

    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

impl std::error::Error for MvarError {}

impl From<std::io::Error> for MvarError {
    fn from(e: std::io::Error) -> Self {
        MvarError::Io(e)
    }
}

// =============================================================================
// Phase 3 (préambule) — Purge des commentaires CSS AVANT tout le reste.
//
// Bug signalé en session : une `$variable` indéfinie ou mal formatée à
// l'intérieur d'un commentaire (`/* $old-var: 10; */`) faisait échouer le
// build alors que ce texte est de la donnée morte — jamais vue par
// `lightningcss`, elle ne devrait jamais être vue par nos pré-passes non
// plus. Principe DOD direct : éliminer la donnée morte le plus tôt
// possible dans le pipeline, avant que quoi que ce soit d'autre n'ait la
// moindre chance de trébucher dessus.
//
// Piège écarté explicitement (celui qui rend une regex ou un
// `.replace("/*", ...)` naïf incorrects) : `/*` et `*/` sont des
// caractères de contenu parfaitement légaux à l'intérieur d'une chaîne
// CSS — `content: "/*";` ne doit jamais être tronqué. Un automate à trois
// états (Normal / DansChaîne / DansCommentaire) est nécessaire, pas une
// recherche de sous-chaîne.
//
// Branchement : appelé dans LES DEUX passes, pas une seule —
//   - Passe A (`walk_variable_graph`) : sans ça, un `$nom: valeur;` ou un
//     `@import "...";` écrit à l'intérieur d'un commentaire multi-lignes
//     serait toujours capturé par `extract_declarations`/
//     `extract_import_targets` (ni l'un ni l'autre ne connaît la notion
//     de commentaire) — un import commenté ferait planter tout le build
//     sur un fichier "manquant" qui n'a jamais eu vocation à exister.
//   - Passe B (`MvarProvider::read`) : corrige directement le bug
//     rapporté (usage de `$var` dans un commentaire).
// Même cause racine dans les deux cas (scan textuel naïf, aveugle aux
// commentaires) : un seul correctif, appliqué aux deux points d'entrée du
// texte source, pas un correctif ponctuel sur le seul symptôme observé.
// =============================================================================

#[derive(Debug)]
struct CssCommentError(String);

impl fmt::Display for CssCommentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "styles (commentaires) : {}", self.0)
    }
}

impl std::error::Error for CssCommentError {}

/// Purge les commentaires CSS `/* ... */` d'un texte source — automate à
/// trois états, un seul passage O(N) sur les octets, aucune regex.
///
/// États : `Normal` (copie tout), `InString(quote)` (une chaîne CSS est en
/// cours — `/*`/`*/` y sont des caractères ordinaires, jamais des
/// délimiteurs), `InComment` (tout est ignoré jusqu'à `*/`, y compris tout
/// ce qui ressemblerait à une chaîne — CSS n'a pas de commentaires
/// imbriqués, le premier `*/` rencontré ferme, point final).
///
/// Copie par segments (`segment_start..i`), jamais octet par octet : les
/// limites de coupe ne tombent QUE sur des octets ASCII à un seul octet
/// (`/`, `*`, `"`, `'`, `\`), donc toujours des frontières de caractère
/// UTF-8 valides — un contenu non-ASCII (accents dans un commentaire ou
/// une chaîne) traverse la fonction sans risque de corruption, puisqu'il
/// n'est jamais reconstruit octet par octet.
///
/// Échec dur sur chaîne ou commentaire non fermé en fin de fichier — un
/// tel fichier est de toute façon invalide, mieux vaut le signaler ici
/// qu'obtenir un message d'erreur incompréhensible plus loin dans le
/// pipeline.
fn strip_css_comments(input: &str) -> Result<String, CssCommentError> {
    enum State {
        Normal,
        InString(u8),
        InComment,
    }

    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut state = State::Normal;
    let mut i = 0usize;
    let mut segment_start = 0usize;

    while i < bytes.len() {
        match state {
            State::Normal => {
                if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    out.push_str(&input[segment_start..i]);
                    state = State::InComment;
                    i += 2;
                } else if bytes[i] == b'"' || bytes[i] == b'\'' {
                    state = State::InString(bytes[i]);
                    i += 1;
                } else {
                    i += 1;
                }
            }
            State::InString(quote) => {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    // Paire échappée (ex. `\"`) : avancée ensemble, jamais
                    // interprétée séparément — un guillemet échappé ne
                    // ferme jamais la chaîne prématurément.
                    i += 2;
                } else if bytes[i] == quote {
                    state = State::Normal;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            State::InComment => {
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    i += 2;
                    state = State::Normal;
                    segment_start = i; // reprise de la copie après le commentaire
                } else {
                    i += 1;
                }
            }
        }
    }

    match state {
        State::Normal => {
            out.push_str(&input[segment_start..]);
            Ok(out)
        }
        State::InString(_) => Err(CssCommentError(
            "chaîne non fermée (guillemet manquant avant la fin du fichier)".to_string(),
        )),
        State::InComment => Err(CssCommentError(
            "commentaire non fermé ('*/' manquant avant la fin du fichier)".to_string(),
        )),
    }
}

/// Passe A — walk textuel minimal du graphe `@import`, lecture seule.
///
/// Ne passe jamais par `lightningcss` : un scan ligne-à-ligne suffit, la
/// seule information recherchée est (a) les déclarations `$nom: valeur;`
/// et (b) les cibles `@import "chemin";` à suivre récursivement. Aucune
/// sémantique `layer(...)`/media n'est interprétée ici — seul le chemin
/// importé compte, la sémantique réelle reste intégralement déléguée à
/// `Bundler` en Passe B.
///
/// Hypothèse de grammaire posée explicitement (non vérifiée par l'auteur
/// à ce stade, à confirmer) : une déclaration `$nom: valeur;` tient sur
/// une seule ligne. Aucun `.mcss` réel ne contredit cette hypothèse au
/// moment de l'écriture (cf. Handoff §1 : aucun fichier de test n'utilise
/// encore de variables).
fn build_variable_registry(entry: &Path) -> Result<VariableRegistry, Box<dyn std::error::Error>> {
    let mut registry = VariableRegistry::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    walk_variable_graph(entry, &mut registry, &mut visited)?;
    Ok(registry)
}

fn walk_variable_graph(
    path: &Path,
    registry: &mut VariableRegistry,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Clé de dédoublonnage canonique — un même partial importé deux fois
    // (diamant d'imports) ne doit être ni relu, ni source d'une boucle
    // infinie sur un cycle d'imports.
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(key) {
        return Ok(());
    }

    let text = fs::read_to_string(path).map_err(|e| {
        format!(
            "styles (variables) : lecture impossible de {} : {e}",
            path.display()
        )
    })?;

    // Purge des commentaires AVANT toute recherche de déclaration ou
    // d'import — voir bloc de commentaires "Phase 3 (préambule)" plus
    // haut : sans ça, un `@import` commenté ferait planter le build sur
    // un fichier qui n'a jamais eu vocation à être lu.
    let text = strip_css_comments(&text)
        .map_err(|e| format!("styles (variables) : {} : {e}", path.display()))?;

    extract_declarations(&text, registry);

    for import_rel in extract_import_targets(&text) {
        // Même règle de résolution que `FileProvider::resolve` (spec :
        // chemin relatif au fichier important, jamais à la racine du
        // thème) — dupliquée ici volontairement : la Passe A n'a pas
        // accès à `Bundler`, mais doit rester cohérente avec sa
        // convention de résolution de chemin.
        let import_path = path.with_file_name(&import_rel);
        walk_variable_graph(&import_path, registry, visited)?;
    }

    Ok(())
}

/// Extrait les déclarations `$nom: valeur;` d'un texte source, une par
/// ligne. Purement additif sur `registry` — dernière déclaration lue
/// l'emporte en cas de redéfinition inter-fichiers (portée globale,
/// cohérent avec l'absence de toute notion de scope/import qualifié dans
/// la spec actuelle du dialecte `$variable`).
fn extract_declarations(text: &str, registry: &mut VariableRegistry) {
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix('$') else {
            continue;
        };
        let Some(colon) = rest.find(':') else {
            continue;
        };
        let name = rest[..colon].trim();
        let Some(value) = rest[colon + 1..].trim().strip_suffix(';') else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let value = value.trim();
        registry.insert(name.to_string(), strip_enclosing_parens(value));
    }
}

/// Dépouille un bloc parenthésé englobant sur la valeur brute d'une
/// déclaration `$var` (Handoff §2 — décision actée : application
/// systématique, pas de nouvelle syntaxe de déclaration). Permet
/// d'écrire `$breakpoint: (35em < width <= 50em);` et de le référencer
/// nu dans `@custom-media`/`@container` via `($breakpoint)`.
///
/// Sûr par absence de collision grammaticale réelle, pas seulement par
/// fiabilité de l'algorithme de comptage : en CSS, une valeur qui
/// commence *littéralement* par `(` — sans nom de fonction juste devant,
/// donc ni `calc(`, ni `rgb(`, ni `repeat(`, ni aucune fonction CSS
/// existante ou probable — n'a essentiellement qu'un seul emplacement
/// légitime dans la grammaire : les conditions de feature-query. Aucune
/// propriété CSS standard n'a de valeur parenthésée nue.
///
/// Un seul niveau de dépouillement, jamais récursif : `((a))` devient
/// `(a)`, pas `a` — aucun cas d'usage fourni ne demande un double
/// enrobage, ne pas résoudre un problème non posé.
fn strip_enclosing_parens(value: &str) -> String {
    if !(value.starts_with('(') && value.ends_with(')')) {
        return value.to_string();
    }
    let bytes = value.as_bytes();
    // Comptage de profondeur, même idiome que `find_matching_brace` : la
    // profondeur ne doit retomber à 0 qu'au tout dernier caractère. Un
    // retour à 0 avant la fin (`(a) + (b)`) signifie que la parenthèse
    // ouvrante en tête et la parenthèse fermante en queue ne sont PAS
    // appariées l'une à l'autre — ne pas strip dans ce cas, la valeur
    // traverse inchangée (sans risque réel : ce cas n'a de toute façon
    // jamais été rencontré dans les `.mcss` fournis à ce jour).
    match find_matching_paren(bytes, 0) {
        Some(close_idx) if close_idx == bytes.len() - 1 => {
            // Sous-chaîne interne, parenthèses exclues, re-trim pour
            // absorber un éventuel `( 35em < width <= 50em )` avec
            // espaces internes.
            value[1..close_idx].trim().to_string()
        }
        _ => value.to_string(),
    }
}

/// Comptage de parenthèses — même principe que `find_matching_brace`
/// (plus bas dans ce fichier) mais sur `(`/`)` au lieu de `{`/`}`.
/// `open_pos` pointe sur la '(' d'ouverture ; retourne l'indice de la
/// ')' fermante correspondante (profondeur 0).
fn find_matching_paren(bytes: &[u8], open_pos: usize) -> Option<usize> {
    let mut depth: i32 = 1;
    let mut i = open_pos + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Extrait les cibles `@import "chemin";` (ou `@import url("chemin");`)
/// d'un texte source. Ne traite que ce dont la Passe A a besoin : le
/// chemin. Les qualificatifs (`layer(...)`, media, supports) sont ignorés
/// ici sans risque — ils ne changent jamais QUEL fichier est importé,
/// seulement comment `Bundler` l'enveloppera en Passe B.
fn extract_import_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("@import") {
            continue;
        }
        let rest = &trimmed["@import".len()..];
        let Some(start) = rest.find(['"', '\'']) else {
            continue;
        };
        let quote = rest.as_bytes()[start] as char;
        let Some(end_rel) = rest[start + 1..].find(quote) else {
            continue;
        };
        targets.push(rest[start + 1..start + 1 + end_rel].to_string());
    }
    targets
}

/// Substitue chaque `$nom` par sa valeur résolue et purge les lignes de
/// déclaration (grammaire CSS fermée, §10.3 de la spec — un token `$nom`
/// non substitué ferait échouer `lightningcss` de toute façon ; le purger
/// en amont est la seule option, pas un choix parmi d'autres).
fn substitute_and_purge(
    text: &str,
    registry: &VariableRegistry,
    file: &Path,
) -> Result<String, MvarError> {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        // Ligne de déclaration : déjà capturée en Passe A, purgée ici
        // pour ne jamais atteindre le parseur (grammaire non reconnue).
        if trimmed.starts_with('$') && trimmed.contains(':') {
            continue;
        }
        out.push_str(&substitute_line(line, registry, file)?);
        out.push('\n');
    }
    Ok(out)
}

/// Substitution caractère-à-caractère d'une seule ligne — un seul passage,
/// aucune allocation intermédiaire hors la chaîne de sortie elle-même.
/// Opère sur `char_indices` (pas `bytes[i] as char`) : une valeur UTF-8
/// multioctet dans un `$nom` de variable romprait un découpage par octet.
fn substitute_line(
    line: &str,
    registry: &VariableRegistry,
    file: &Path,
) -> Result<String, MvarError> {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();

    while let Some((idx, c)) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }

        let start = idx + c.len_utf8();
        let mut end = start;
        while let Some(&(j, ch)) = chars.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                end = j + ch.len_utf8();
                chars.next();
            } else {
                break;
            }
        }

        if end == start {
            // '$' isolé, sans nom derrière : pas une variable, recopié tel quel.
            out.push(c);
            continue;
        }

        let name = &line[start..end];
        match registry.get(name) {
            Some(value) => out.push_str(value),
            None => {
                return Err(MvarError::UndefinedVariable {
                    name: name.to_string(),
                    file: file.to_path_buf(),
                    suggestion: suggest_variable(name, registry),
                });
            }
        }
    }

    Ok(out)
}

// =============================================================================
// Phase 3 (suite) — Déroulage AOT des boucles `@for` (dialecte Sass-like).
//
// Différence structurelle avec le registre de $variables plates ci-dessus :
// une boucle `@for` est ENTIÈREMENT locale à son fichier — borne, pas et
// corps sont dans le même texte. Pas de piège d'ordre inter-fichiers ici,
// donc pas de Passe A dédiée : le déroulage tient dans le point
// d'interception déjà en place (`MvarProvider::read`).
//
// Ordre à l'intérieur de `read()` (voir plus bas) :
//   1. `expand_for_loops`     — élimine tout `@for`, ne substitue QUE la
//                                variable de boucle courante ($i / $(i)),
//                                laisse tout autre `$nom` strictement
//                                intact.
//   2. `substitute_and_purge` — résout les `$nom` globaux restants via le
//                                VariableRegistry de la Passe A.
// Ne jamais inverser : `substitute_and_purge` échoue dur sur tout `$nom`
// non déclaré — inversé, elle verrait encore `$i` et le traiterait comme
// une variable globale absente. C'est exactement l'erreur observée
// (`UndefinedVariable("i", …)`) avant ce correctif.
// =============================================================================

#[derive(Debug)]
struct ForLoopError(String);

impl fmt::Display for ForLoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "styles (@for) : {}", self.0)
    }
}

impl std::error::Error for ForLoopError {}

/// Déroule tous les `@for $var from A to B [by S] { ... }` d'un texte.
/// Récursif : le corps isolé (comptage d'accolades, pas de regex) est
/// entièrement déplié AVANT d'être dupliqué par la boucle englobante — une
/// boucle imbriquée est donc traitée une seule fois, pas de second passage
/// nécessaire. Limite assumée : deux boucles imbriquées partageant le même
/// nom de variable ($i dans les deux) ne sont pas gardées contre une
/// collision — cas non rencontré dans les fichiers actuels, à traiter si
/// besoin réel se présente.
///
/// Hypothèse de grammaire à confirmer explicitement : `to` est ici traité
/// comme EXCLUSIF de la borne haute (convention Sass standard — `through`
/// serait inclusif, mais n'apparaît pas dans la grammaire cible donnée).
/// Conséquence concrète sur votre second exemple : `@for $i from 90 to
/// 180 by 90` ne produit qu'UNE itération (i = 90, 180 exclu) avec cette
/// hypothèse. Si vous attendiez `rotate90` ET `rotate180`, c'est `to`
/// inclusif qu'il faut — je ne tranche pas ce point à votre place, à
/// confirmer avant de considérer cette Phase 3 close.
fn expand_for_loops(text: &str) -> Result<String, ForLoopError> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;

    while let Some(rel) = text[cursor..].find("@for") {
        let for_start = cursor + rel;
        out.push_str(&text[cursor..for_start]);

        let mut i = for_start + "@for".len();
        i = skip_ws(bytes, i);

        i = expect_byte(bytes, i, b'$')
            .ok_or_else(|| ForLoopError(format!("'$' attendu après @for (position {i})")))?;
        let (var_name, next) = parse_ident(text, i).ok_or_else(|| {
            ForLoopError(format!("nom de variable attendu après '$' (position {i})"))
        })?;
        i = skip_ws(bytes, next);

        i = expect_literal(text, i, "from")
            .ok_or_else(|| ForLoopError(format!("mot-clé 'from' attendu (position {i})")))?;
        i = skip_ws(bytes, i);
        let (start, next) = parse_int(text, i)
            .ok_or_else(|| ForLoopError(format!("borne basse entière attendue (position {i})")))?;
        i = skip_ws(bytes, next);

        i = expect_literal(text, i, "to")
            .ok_or_else(|| ForLoopError(format!("mot-clé 'to' attendu (position {i})")))?;
        i = skip_ws(bytes, i);
        let (end, next) = parse_int(text, i)
            .ok_or_else(|| ForLoopError(format!("borne haute entière attendue (position {i})")))?;
        i = skip_ws(bytes, next);

        let mut step: i64 = 1;
        if let Some(after_by) = expect_literal(text, i, "by") {
            let after_ws = skip_ws(bytes, after_by);
            let (s, next) = parse_int(text, after_ws).ok_or_else(|| {
                ForLoopError(format!(
                    "pas entier attendu après 'by' (position {after_ws})"
                ))
            })?;
            if s == 0 {
                return Err(ForLoopError("pas ('by') ne peut pas être 0".to_string()));
            }
            step = s;
            i = skip_ws(bytes, next);
        }

        i = expect_byte(bytes, i, b'{').ok_or_else(|| {
            ForLoopError(format!(
                "'{{' attendu pour ouvrir le corps de boucle (position {i})"
            ))
        })?;

        let body_start = i;
        let body_end = find_matching_brace(bytes, body_start)
            .ok_or_else(|| ForLoopError("accolade fermante manquante pour @for".to_string()))?;
        let raw_body = &text[body_start..body_end];

        // Récursion AVANT duplication : toute boucle imbriquée dans le
        // corps est entièrement dépliée une seule fois ici.
        let expanded_body = expand_for_loops(raw_body)?;

        let mut i_iter = start;
        loop {
            let done = if step > 0 {
                i_iter >= end
            } else {
                i_iter <= end
            };
            if done {
                break;
            }
            out.push_str(&substitute_loop_variable(&expanded_body, &var_name, i_iter));
            i_iter += step;
        }

        cursor = body_end + 1; // juste après la '}' fermante du @for
    }

    out.push_str(&text[cursor..]);
    Ok(out)
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    i
}

fn expect_byte(bytes: &[u8], i: usize, b: u8) -> Option<usize> {
    if i < bytes.len() && bytes[i] == b {
        Some(i + 1)
    } else {
        None
    }
}

fn expect_literal(text: &str, i: usize, lit: &str) -> Option<usize> {
    text.get(i..)?.strip_prefix(lit).map(|_| i + lit.len())
}

fn parse_ident(text: &str, i: usize) -> Option<(String, usize)> {
    let rest = text.get(i..)?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    if end == 0 {
        None
    } else {
        Some((rest[..end].to_string(), i + end))
    }
}

fn parse_int(text: &str, i: usize) -> Option<(i64, usize)> {
    let rest = text.get(i..)?;
    let bytes = rest.as_bytes();
    let mut end = 0;
    if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
        end += 1;
    }
    let digits_start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == digits_start {
        return None;
    }
    rest[..end].parse::<i64>().ok().map(|v| (v, i + end))
}

/// Comptage d'accolades — pas de regex, la grammaire n'est pas régulière
/// (le corps contient ses propres règles CSS imbriquées avec `{`/`}`).
/// `open_pos` pointe sur la '{' d'ouverture ; retourne l'indice de la '}'
/// fermante correspondante (profondeur 0).
fn find_matching_brace(bytes: &[u8], open_pos: usize) -> Option<usize> {
    let mut depth: i32 = 1;
    let mut i = open_pos + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Remplace UNIQUEMENT `$(nom)` et `$nom` (frontière de mot stricte) par
/// la valeur de l'itération courante — tout autre `$token` (variable
/// globale pas encore résolue, ex. `$demoColorDeviation`) traverse
/// strictement inchangé. Volontairement non-erronant sur un `$autre`
/// rencontré : ce n'est pas son rôle, `substitute_and_purge` s'en charge
/// en aval, une fois le registre global connu.
fn substitute_loop_variable(body: &str, var_name: &str, value: i64) -> String {
    let value_str = value.to_string();
    let mut out = String::with_capacity(body.len());
    let mut i = 0usize;

    while i < body.len() {
        if body.as_bytes()[i] != b'$' {
            let next_dollar = body[i..].find('$').map(|r| i + r).unwrap_or(body.len());
            out.push_str(&body[i..next_dollar]);
            i = next_dollar;
            continue;
        }

        // Forme interpolée : $(nom)
        if body
            .get(i + 1..)
            .map(|s| s.starts_with('('))
            .unwrap_or(false)
        {
            let name_start = i + 2;
            if let Some(close_rel) = body[name_start..].find(')') {
                let name_end = name_start + close_rel;
                let name = &body[name_start..name_end];
                if name == var_name {
                    out.push_str(&value_str);
                } else {
                    // $(autre_nom) : pas notre variable, recopié tel quel.
                    out.push_str(&body[i..name_end + 1]);
                }
                i = name_end + 1;
                continue;
            }
            // '(' sans ')' fermante : pas une interpolation valide, '$'
            // recopié seul, le reste suit son cours normalement.
            out.push('$');
            i += 1;
            continue;
        }

        // Forme nue : $nom, frontière de mot stricte.
        let name_start = i + 1;
        let name_end = body[name_start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .map(|r| name_start + r)
            .unwrap_or(body.len());

        if name_end > name_start {
            let name = &body[name_start..name_end];
            if name == var_name {
                out.push_str(&value_str);
            } else {
                out.push_str(&body[i..name_end]);
            }
            i = name_end;
        } else {
            out.push('$');
            i += 1;
        }
    }

    out
}

/// Passe B — `SourceProvider` custom, remplace `FileProvider` en entrée de
/// `Bundler`. Seul point d'interception : chaque fichier du graphe,
/// entrée comme import résolu par `Bundler` lui-même, transite par
/// `read()` avant tout parsing — la substitution y est donc appliquée de
/// façon globale et transparente sans dupliquer la logique d'import de
/// `Bundler`.
///
/// Contrainte de signature à respecter strictement :
/// `read<'a>(&'a self, file: &Path) -> Result<&'a str, Self::Error>` — la
/// référence retournée est liée à la durée de vie de `&self`, pas de
/// l'appel. Un `String` local ne peut donc pas être retourné par `&str`
/// sans que son adresse reste stable après la fin de `read()`. Solution
/// identique à celle déjà employée par `FileProvider` lui-même (lu dans
/// le code source du crate, §"Point de vigilance" du Handoff toujours
/// valable) : `Box::into_raw` + accumulation des pointeurs, libérés au
/// `Drop` de `MvarProvider`, jamais retirés du `Vec` entre-temps.
struct MvarProvider {
    registry: VariableRegistry,
    outputs: Mutex<Vec<*mut String>>,
    // §1.A — contenu de remplacement pour le fichier d'entrée exclusivement
    // (chemin canonique + texte avec `@charset` déjà extrait par
    // `transform_css` avant l'appel à `Bundler::bundle()`). `None` si le
    // fichier d'entrée ne portait pas de `@charset` en tête — dans ce cas
    // `read()` retombe sur la lecture disque normale, aucun comportement
    // différent. Ne s'applique jamais aux imports (partials) : seule la
    // clé `entry_path` peut matcher, cf. Handoff §1.A, "Ne pas chercher le
    // @charset dans les fichiers importés".
    entry_override: Option<(PathBuf, String)>,
}

impl MvarProvider {
    fn new(registry: VariableRegistry, entry_override: Option<(PathBuf, String)>) -> Self {
        MvarProvider {
            registry,
            outputs: Mutex::new(Vec::new()),
            entry_override,
        }
    }
}

// SAFETY : même justification que `FileProvider` dans lightningcss —
// aucun état mutable partagé n'est exposé sans passer par le `Mutex`, et
// les pointeurs accumulés ne sont jamais déréférencés en dehors de ce
// fichier ni retirés avant le `Drop`.
unsafe impl Sync for MvarProvider {}
unsafe impl Send for MvarProvider {}

impl SourceProvider for MvarProvider {
    type Error = MvarError;

    fn read<'a>(&'a self, file: &Path) -> Result<&'a str, Self::Error> {
        // §1.A — seul le fichier d'entrée peut matcher `entry_override`
        // (comparaison directe, sans canonicalisation : `file` est ici
        // exactement le `Path` transmis à `Bundler::bundle()` lors du
        // premier appel de ce cycle de bundling, donc bit-identique à la
        // clé stockée — aucun risque de non-appariement par forme de
        // chemin différente, contrairement à `walk_variable_graph` qui,
        // lui, canonicalise explicitement pour dédupliquer un graphe
        // d'imports). Les imports résolus par `resolve()` ci-dessous ne
        // passeront jamais ce test, par construction : ce ne sont jamais
        // le même `Path` que `entry_path`.
        let raw = match &self.entry_override {
            Some((entry, content)) if entry == file => content.clone(),
            _ => fs::read_to_string(file)?,
        };
        // Ordre impératif, trois étapes : purge des commentaires D'ABORD
        // (donnée morte éliminée avant que quoi que ce soit d'autre ne la
        // voie — voir "Phase 3 (préambule)" plus haut), déroulage des @for
        // ENSUITE (élimine $i / $(i) sans toucher aux $vars globales),
        // résolution du registre global EN DERNIER (voir "Phase 3
        // (suite)" plus haut).
        let stripped = strip_css_comments(&raw).map_err(MvarError::Comment)?;
        let unrolled = expand_for_loops(&stripped).map_err(MvarError::ForLoop)?;
        let transformed = substitute_and_purge(&unrolled, &self.registry, file)?;
        let ptr = Box::into_raw(Box::new(transformed));
        self.outputs.lock().unwrap().push(ptr);
        // SAFETY : le pointeur ne meurt qu'au `Drop` de `MvarProvider`, et
        // n'est jamais retiré du `Vec` avant — la référence rendue reste
        // valide aussi longtemps que `&'a self`.
        Ok(unsafe { &*ptr })
    }

    fn resolve(
        &self,
        specifier: &str,
        originating_file: &Path,
    ) -> Result<ResolveResult, Self::Error> {
        // Résolution de chemin identique à `FileProvider::resolve` — la
        // Phase 3 ne change pas la convention de résolution des imports,
        // seulement le contenu texte renvoyé pour chaque fichier.
        Ok(originating_file.with_file_name(specifier).into())
    }
}

impl Drop for MvarProvider {
    fn drop(&mut self) {
        for ptr in self.outputs.lock().unwrap().iter() {
            drop(unsafe { Box::from_raw(*ptr) });
        }
    }
}

/// Extrait un `@charset "...";` en tout début de fichier (Handoff §1.A).
///
/// Détection strictement alignée sur la contrainte W3C (octet 0 exact,
/// aucun BOM/espace/commentaire avant) : après un simple `trim_start`
/// (pas de recherche en profondeur), si le premier caractère non-blanc
/// du fichier n'est pas littéralement `@charset`, ce n'est de toute
/// façon pas un `@charset` valide au sens de la spec — dans ce cas
/// aucune extraction n'a lieu, et le texte original est renvoyé
/// inchangé (`None`, texte original).
///
/// Si trouvé : retourne `(Some(règle_avec_point_virgule), reste_du_texte)`.
/// `@charset` est une règle simple, sans bloc imbriqué (`{`/`}` ou `(`/`)`)
/// — un scan naïf du premier `;` suffit, pas besoin de comptage de
/// profondeur ici (contrairement à `find_matching_brace`/
/// `find_matching_paren` ailleurs dans ce fichier).
fn extract_charset(text: &str) -> (Option<String>, String) {
    let trimmed_start = text.trim_start();
    if !trimmed_start.starts_with("@charset") {
        return (None, text.to_string());
    }
    match trimmed_start.find(';') {
        Some(semi_idx) => {
            let rule = trimmed_start[..=semi_idx].to_string();
            let remainder = trimmed_start[semi_idx + 1..].to_string();
            (Some(rule), remainder)
        }
        // `@charset` sans `;` terminal : @-rule malformée. On ne
        // spécialise rien — laisser `lightningcss` produire son propre
        // diagnostic de parsing plutôt que de deviner une intention.
        None => (None, text.to_string()),
    }
}

/// Pipeline `[styles]` réel — spec §10.1, §10.3, Roadmap §1.8 (tranchée).
///
/// 1. Bundling (`Bundler` + `MvarProvider`) : résout et inline les
///    `@import`, y compris ceux qualifiés `layer(...)`. **Écart assumé par
///    rapport à la demande initiale** : je n'ai pas implémenté de logique
///    séparée pour "préserver" les imports en couche sans les inliner — un
///    `@import` non résolu resterait une requête réseau non hachée, non
///    présente dans le manifeste, ce qui romprait l'invariant de
///    versionnement de tout ce pipeline. `layer(...)` qualifie la couche
///    cible du contenu importé, pas une exemption d'inlining : un bundler
///    conforme à la spec CSS Cascade Layers inline le contenu et
///    l'enveloppe dans le `@layer` nommé, il ne le laisse pas non résolu.
///    Le `Bundler` standard fait déjà cela — aucun traitement spécial requis.
/// 2. Visiteur AST (`CssUrlVisitor` ci-dessus) : validation dure +
///    réécriture d'URL pour TOUT `url()` du document — plus seulement
///    `@font-face` (Roadmap §1.8, désormais tranchée par la demande de
///    session : `background-image` doit être réécrit exactement comme
///    `@font-face` l'était déjà).
/// 3. Minification, puis émission du CSS final.
///
/// **Pourquoi la réécriture d'URL se fait ICI, avant minification/hash, et
/// pas "en toute fin de build"** (question explicitement posée en
/// session) : le hash du fichier CSS produit (`run_styles_pipeline`, juste
/// après l'appel à cette fonction) doit refléter EXACTEMENT ce qui est
/// servi — invariant déjà en place pour `@font-face` avant cette session,
/// pas une nouveauté. Réécrire après coup (sur le fichier déjà écrit et
/// haché) obligerait soit à re-hacher après coup (passe supplémentaire,
/// aucun bénéfice sur la première option), soit à accepter un hash
/// obsolète (romprait l'invariant). Faire la réécriture ICI, avant
/// `minify`/`to_css`, ne coûte rien de plus qu'un second passage du même
/// visiteur déjà en place — la seule différence est la portée
/// (`in_font_face` retiré), pas le moment.
///
/// Pré-passe lexicale des commentaires, des variables `$` et des boucles
/// `@for` (Phase 3) : dans `MvarProvider::read`, `strip_css_comments`
/// d'abord (donnée morte éliminée avant tout le reste — un `$var`
/// indéfinie ou un `@for` malformé À L'INTÉRIEUR d'un commentaire ne doit
/// jamais faire échouer le build), puis `expand_for_loops` (élimine
/// `@for`, local à chaque fichier, pas de piège d'ordre), puis résolution
/// des `$nom` globaux via le `VariableRegistry` construit en amont par
/// `build_variable_registry` (walk textuel du graphe `@import`, AVANT que
/// `Bundler` ne lise quoi que ce soit — piège d'ordre inter-fichiers,
/// celui-là bien réel, évité par cette séparation ; cette Passe A
/// applique elle aussi `strip_css_comments` en premier, même raison). Voir
/// les blocs de commentaires "Phase 3" plus haut dans ce fichier pour le
/// raisonnement complet.
///
/// Note de version — confirmé par compilation réelle (retour de session,
/// `lightningcss = "=1.0.0-alpha.71"`) : `ParserOptions` se passe à
/// `Bundler::new()` (3 arguments), pas à `.bundle()` (1 seul argument, le
/// chemin). L'ancienne version de ce commentaire supposait l'inverse par
/// prudence documentaire, faute de pouvoir compiler dans cet
/// environnement — l'ambiguïté est levée, plus un avertissement.
fn transform_css(
    theme_dir: &Path,
    entry_path: &Path,
    asset_url_registry: &AssetUrlRegistry,
) -> Result<String, Box<dyn std::error::Error>> {
    // Passe A — walk textuel complet du graphe AVANT toute chose : le
    // registre doit être figé pour tout le graphe avant que `Bundler` ne
    // lise ne serait-ce que le fichier d'entrée (cf. commentaire Phase 3
    // ci-dessus — piège d'ordre si cette étape était fusionnée avec la
    // lecture individuelle de chaque fichier).
    let var_registry = build_variable_registry(entry_path)?;

    // §1.A — extraction du `@charset` du fichier d'entrée AVANT tout
    // passage dans `Bundler`/`lightningcss`. Cause confirmée par lecture
    // directe du source du crate (`lightningcss-1.0.0-alpha.71/src/parser.rs`,
    // commentaire du mainteneur) : `@charset` est systématiquement
    // supprimé par `rust-cssparser`, qu'il soit en tête de fichier ou
    // ailleurs — aucun comportement interne de `lightningcss` à ce sujet
    // n'est fiable, donc la solution retenue l'évite complètement en ne
    // lui laissant jamais voir la règle. Réinjection en toute fin de
    // cette fonction, en texte brut, avant le `Ok(...)` final.
    let raw_entry = fs::read_to_string(entry_path).map_err(|e| {
        format!(
            "styles : lecture impossible du point d'entrée {} : {e}",
            entry_path.display()
        )
    })?;
    let (charset_rule, entry_without_charset) = extract_charset(&raw_entry);
    let entry_override = charset_rule
        .as_ref()
        .map(|_| (entry_path.to_path_buf(), entry_without_charset));

    // Passe B — `Bundler` s'exécute normalement, mais chaque lecture de
    // fichier passe par `MvarProvider` : substitution + purge transparentes,
    // `Bundler` ne voit jamais un seul token `$`.
    let provider = MvarProvider::new(var_registry, entry_override);
    let parser_options = ParserOptions::default();
    let mut bundler = Bundler::new(&provider, None, parser_options);
    let mut stylesheet = bundler.bundle(entry_path).map_err(|e| {
        format!(
            "styles : bundling échoué pour {} : {e:?}",
            entry_path.display()
        )
    })?;

    // Clone nécessaire : `sources` doit être détaché de `stylesheet` AVANT
    // la construction du visiteur, sinon `visitor` porte un emprunt
    // immuable À L'INTÉRIEUR de `stylesheet` (son champ `sources`), ce qui
    // interdit ensuite `stylesheet.visit(&mut visitor)` (emprunt mutable
    // de `stylesheet` en conflit direct avec l'emprunt immuable déjà
    // détenu par `visitor`). Un `Vec<String>` cloné une fois par build,
    // jamais sur le chemin chaud — coût négligeable.
    let sources_owned: Vec<String> = stylesheet.sources.clone();

    let mut visitor = CssUrlVisitor {
        registry: asset_url_registry,
        theme_dir,
        sources: &sources_owned,
        current_source_index: None,
    };
    stylesheet
        .visit(&mut visitor)
        .map_err(|e| format!("styles : {e}"))?;

    stylesheet.minify(MinifyOptions::default()).map_err(|e| {
        format!(
            "styles : minification échouée pour {} : {e:?}",
            entry_path.display()
        )
    })?;

    let result = stylesheet
        .to_css(PrinterOptions {
            minify: true,
            ..Default::default()
        })
        .map_err(|e| {
            format!(
                "styles : émission échouée pour {} : {e:?}",
                entry_path.display()
            )
        })?;

    // §1.A — dernière étape : réinjection en tête, en texte brut, du
    // `@charset` capturé plus haut. `lightningcss` ne l'a jamais vu, donc
    // ne peut pas l'avoir supprimé ; il occupe ici l'octet 0 exact du
    // fichier réellement écrit sur disque, conformément à la contrainte
    // W3C.
    match charset_rule {
        Some(rule) => Ok(format!("{rule}{}", result.code)),
        None => Ok(result.code),
    }
}

pub(crate) fn run_styles_pipeline(
    theme_dir: &Path,
    build_root: &Path,
    build_root_rel: &str,
    entries: &[String],
    asset_url_registry: &AssetUrlRegistry,
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<(), Box<dyn std::error::Error>> {
    for rel_path in entries {
        let source_path = theme_dir.join(rel_path);
        if !source_path.is_file() {
            return Err(format!("styles : fichier introuvable : {}", source_path.display()).into());
        }

        let transformed = transform_css(theme_dir, &source_path, asset_url_registry)?;
        let bytes = transformed.as_bytes();
        let (full_hash, short_hash) = hash_content(bytes);

        let rel = Path::new(rel_path);
        let stem = rel
            .file_stem()
            .ok_or_else(|| format!("styles : nom de fichier invalide : {rel_path}"))?
            .to_string_lossy();

        let logical_key = format!("{stem}.css");
        let hashed_filename = format!("{stem}.{short_hash}.css");
        let output_rel = join_slash("styles", &hashed_filename);
        let output_abs = build_root.join(&output_rel);

        if let Some(dir) = output_abs.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&output_abs, bytes)?;

        manifest.insert(
            logical_key,
            AssetEntry {
                url: format!("/{output_rel}"),
                path: join_slash(build_root_rel, &output_rel),
                mime: mime_for_extension("css").to_string(),
                size: bytes.len() as u64,
                hash: full_hash,
                version: String::new(),
            },
        );

        println!("[marius-assets] styles    {rel_path} -> /{output_rel}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_css_comments (Phase 3, préambule) ─────────────────────────────

    #[test]
    fn strip_css_comments_removes_simple_comment() {
        let input = ".a { color: red; /* comment */ }";
        assert_eq!(strip_css_comments(input).unwrap(), ".a { color: red;  }");
    }

    #[test]
    fn strip_css_comments_multiline_comment_is_removed_entirely() {
        let input = "before\n/*\nmulti\nline\n*/\nafter";
        assert_eq!(strip_css_comments(input).unwrap(), "before\n\nafter");
    }

    /// Exemple exact de la mission : `/*` à l'intérieur d'une chaîne CSS
    /// n'est pas un délimiteur de commentaire, cette propriété ne doit
    /// jamais être altérée.
    #[test]
    fn strip_css_comments_preserves_slash_star_inside_double_quoted_string() {
        let input = r#".icon::before { content: "/*"; }"#;
        assert_eq!(strip_css_comments(input).unwrap(), input);
    }

    #[test]
    fn strip_css_comments_preserves_slash_star_inside_single_quoted_string() {
        let input = ".icon::before { content: '/*'; }";
        assert_eq!(strip_css_comments(input).unwrap(), input);
    }

    /// Un guillemet échappé à l'intérieur de la chaîne ne doit jamais être
    /// vu comme sa fermeture — sinon le `/*` qui suit serait interprété
    /// hors chaîne et supprimerait à tort le reste du fichier.
    #[test]
    fn strip_css_comments_escaped_quote_does_not_close_string_early() {
        let input = "content: \"a\\\" /* b\"; ";
        assert_eq!(strip_css_comments(input).unwrap(), input);
    }

    /// Le bug exact rapporté en session : une `$variable` indéfinie à
    /// l'intérieur d'un commentaire ne doit plus jamais atteindre
    /// `substitute_line` — la preuve la plus directe est qu'elle a
    /// entièrement disparu du texte après cette passe.
    #[test]
    fn strip_css_comments_hides_undefined_variable_usage_inside_comment() {
        let input = ".a { color: red; /* $old-var: 10; */ }";
        let stripped = strip_css_comments(input).unwrap();
        assert!(!stripped.contains('$'));
    }

    #[test]
    fn strip_css_comments_unterminated_comment_is_an_error() {
        assert!(strip_css_comments(".a { /* never closed").is_err());
    }

    #[test]
    fn strip_css_comments_unterminated_string_is_an_error() {
        assert!(strip_css_comments("content: \"never closed").is_err());
    }

    // ── suggest_variable / levenshtein (Phase 3, $variables) ────────────────

    fn registry_with(pairs: &[(&str, &str)]) -> VariableRegistry {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn levenshtein_identical_strings_is_zero() {
        assert_eq!(levenshtein("demoColorDeg", "demoColorDeg"), 0);
    }

    #[test]
    fn levenshtein_classic_kitten_sitting_is_three() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    /// Le cas réel qui a motivé cette fonctionnalité : une variable saisie
    /// avec une casse différente de sa déclaration.
    #[test]
    fn suggest_variable_case_mismatch_takes_priority_over_levenshtein() {
        let registry = registry_with(&[("demoColorDeg", "15")]);
        let suggestion = suggest_variable("democolordeg", &registry)
            .expect("une entrée ne différant que par la casse doit produire une suggestion");
        assert!(
            suggestion.contains("la casse est sensible"),
            "message inattendu : {suggestion:?}"
        );
        assert!(
            suggestion.contains("demoColorDeg"),
            "message inattendu : {suggestion:?}"
        );
    }

    #[test]
    fn suggest_variable_close_typo_suggests_nearest_key() {
        let registry = registry_with(&[("brandPrimary", "#ff0000")]);
        let suggestion = suggest_variable("brandPrimarry", &registry)
            .expect("distance 1 : une suggestion est attendue");
        assert_eq!(suggestion, "vouliez-vous dire $brandPrimary ?");
    }

    #[test]
    fn suggest_variable_no_close_match_returns_none() {
        let registry = registry_with(&[("brandPrimary", "#ff0000")]);
        assert_eq!(suggest_variable("totallyUnrelatedName", &registry), None);
    }

    #[test]
    fn suggest_variable_empty_registry_returns_none() {
        let registry = VariableRegistry::new();
        assert_eq!(suggest_variable("anything", &registry), None);
    }

    // ── expand_for_loops / substitute_loop_variable (Phase 3, @for) ─────────

    #[test]
    fn substitute_loop_variable_replaces_interpolated_form() {
        assert_eq!(substitute_loop_variable("<a>$(i)</a>", "i", 5), "<a>5</a>");
    }

    #[test]
    fn substitute_loop_variable_replaces_bare_form_at_word_boundary() {
        assert_eq!(substitute_loop_variable("v$i", "i", 5), "v5");
    }

    /// Propriété de non-préfixe : `$i` ne doit jamais matcher à l'intérieur
    /// de `$image` — sans cette frontière de mot stricte, toute variable
    /// dont le nom est un préfixe d'une autre serait corrompue.
    #[test]
    fn substitute_loop_variable_does_not_match_variable_name_as_prefix() {
        assert_eq!(substitute_loop_variable("$image", "i", 5), "$image");
    }

    #[test]
    fn substitute_loop_variable_leaves_other_variables_untouched() {
        assert_eq!(
            substitute_loop_variable("$(other) stays, $other too", "i", 5),
            "$(other) stays, $other too"
        );
    }

    #[test]
    fn substitute_loop_variable_lone_dollar_at_end_is_kept_as_is() {
        assert_eq!(substitute_loop_variable("trailing $", "i", 5), "trailing $");
    }

    #[test]
    fn expand_for_loops_default_step_unrolls_each_integer_exclusive_of_end() {
        // `to` exclusif (convention Sass) : from 1 to 3 → i = 1, 2 seulement.
        let out = expand_for_loops("@for $i from 1 to 3 {<a>$(i)</a>}").unwrap();
        assert_eq!(out, "<a>1</a><a>2</a>");
    }

    #[test]
    fn expand_for_loops_explicit_step_by_is_respected() {
        let out = expand_for_loops("@for $i from 10 to 40 by 10 {<r>$(i)</r>}").unwrap();
        assert_eq!(out, "<r>10</r><r>20</r><r>30</r>");
    }

    #[test]
    fn expand_for_loops_bare_form_inside_calc_is_substituted() {
        let out = expand_for_loops("@for $i from 1 to 3 {v$i}").unwrap();
        assert_eq!(out, "v1v2");
    }

    /// Le bug exact observé en session : un `$nom` global (pas la variable
    /// de boucle) présent dans le corps doit traverser le déroulage
    /// intact — sa résolution est la responsabilité de
    /// `substitute_and_purge`, pas d'`expand_for_loops`.
    #[test]
    fn expand_for_loops_leaves_global_variables_untouched_for_later_pass() {
        let out = expand_for_loops("@for $i from 1 to 2 {a$i b$other c}").unwrap();
        assert_eq!(out, "a1 b$other c");
    }

    /// Boucles imbriquées : l'intérieure doit être entièrement dépliée
    /// avant que l'extérieure ne duplique son corps — sans quoi le texte
    /// dupliqué contiendrait encore un `@for` littéral, jamais réexaminé.
    #[test]
    fn expand_for_loops_nested_loop_is_expanded_before_outer_duplication() {
        let out = expand_for_loops("@for $i from 1 to 2 {@for $j from 1 to 3 {<b>$(i)-$(j)</b>}}")
            .unwrap();
        assert_eq!(out, "<b>1-1</b><b>1-2</b>");
    }

    #[test]
    fn expand_for_loops_missing_to_keyword_is_an_error() {
        assert!(expand_for_loops("@for $i from 1 through 3 {x}").is_err());
    }

    #[test]
    fn expand_for_loops_zero_step_is_an_error() {
        assert!(expand_for_loops("@for $i from 1 to 10 by 0 {x}").is_err());
    }

    #[test]
    fn expand_for_loops_unclosed_brace_is_an_error() {
        assert!(expand_for_loops("@for $i from 1 to 3 {x").is_err());
    }

    #[test]
    fn expand_for_loops_text_without_any_loop_passes_through_unchanged() {
        assert_eq!(
            expand_for_loops(".foo { color: red; }").unwrap(),
            ".foo { color: red; }"
        );
    }
}
