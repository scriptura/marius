// crates/forge/fragment-forge/src/page/model.rs

//! Phase 3.0 — Mode Page, briques structurelles : marqueurs de bloc
//! (`PageBlockToken`), identité d'arène (`TemplateId`), plages nommées
//! (`NamedBlockRange`), spécification enfant (`ChildTemplateSpec`),
//! template parsé (`ParsedPageTemplate`) et son arène (`PageArena`),
//! référence `{% static %}` (`StaticPartialRef`), et les trois familles
//! d'erreurs de phase (parse/link/validation). Types frères de
//! `FlatPageToken`, jamais fusionnés dans son enum gelé — voir doc de tête.

use crate::fragment::lexer::SpanKind;
use crate::page::token::PageSourceToken;

// =============================================================================
// Phase 3.0 — Mode Page, brique structurelle (additif, non câblé)
// =============================================================================
//
// Portée de cette section : trois ajouts de données pures, zéro logique de
// résolution, zéro appel entrant depuis scan / parse_tokens / validate_ast /
// resolve_and_measure / generate_aot_snippet. Ces cinq fonctions restent
// gelées dans cette session — voir HANDOFF-mode-page-brique-structurelle.md.
//
// ─── Décision actée — Bool vs u8 (préalable bloquant du handoff) ─────────────
//
//   Tranché : u8-sentinelle, pas bool natif. `StorageRow` est `#[repr(C)]` et
//   contraint `bytemuck::Pod` ; `bool` n'est pas `Pod` (représentation non
//   garantie sur tous les bits), `u8` l'est. `FlatPageToken::IfBool` a déjà
//   acté ce choix pour le mode fragment (`generate_aot_snippet` émet
//   `if record.{field} != 0`). Le mode page hérite du même contrat : toute
//   condition portée par un futur token de bloc sera un `u8` testé `!= 0`,
//   jamais un `bool`. La spécification v1.1 §8 décrit le type DDL conceptuel
//   (BOOLEAN côté SQL) ; la représentation mémoire côté moteur reste u8.
//   Ce choix est documenté ici et non ré-ouvert au niveau du codegen.
//
// ─── Décision de câblage — pourquoi ces types ne vivent PAS dans FlatPageToken ─
//
//   Le handoff propose un mirror direct de IfBool/EndIf *dans* FlatPageToken.
//   Cette session ne le fait pas : FlatPageToken est un enum matché de façon
//   exhaustive (sans arm `_`) dans validate_ast, resolve_and_measure et
//   generate_aot_snippet — les trois fonctions explicitement gelées. Ajouter
//   une variante à FlatPageToken casse leur exhaustivité et force une édition
//   de ces fonctions pour recompiler, ce qui viole la contrainte de méthode.
//   Les nouveaux types ci-dessous sont donc des types frères, isomorphes à ce
//   qu'exigerait un mirror interne, mais hors de l'enum gelé. La fusion —
//   soit par variante additionnelle sur FlatPageToken, soit par un type de
//   token englobant paramétré sur les deux enums — est une décision de
//   câblage réservée à la session qui écrira le parseur mode page.

/// Marqueur de bloc `{% block %}` / `{% endblock %}` (Mode Page).
///
/// Isomorphe à `FlatPageToken::IfBool` / `EndIf` : ouverture nommée puis
/// fermeture, sans nom porté sur la fermeture (la FSM à un niveau d'état
/// suffit — cf. `current_open_if` dans `validate_ast`, dont le principe se
/// généralise directement à une paire `current_open_block: Option<&str>`
/// dans la validation sémantique mode page, à écrire dans une session
/// ultérieure).
///
/// ─── Invariant de platitude ───────────────────────────────────────────────
///
///   Aucune variante ici ne porte de `Vec<FlatPageToken>` ni de `Vec<Self>`
///   imbriqué. Le contenu d'un bloc reste une plage linéaire d'indices dans
///   l'AST plat existant (`Vec<FlatPageToken<'src>>`), bornée par la position
///   de `BlockOpen` et de `BlockEnd` correspondant. C'est le même principe
///   que la plage implicite entre `IfBool` et `EndIf` : aucun nouvel arbre,
///   aucune récursion de structure.
///
/// ─── Conditions embarquées : tranché "non" pour cette forme ────────────────
///
///   Le handoff pose la question : si `{% block %}` porte lui-même une
///   condition, sa forme dépend du choix bool/u8 ci-dessus. Cette session
///   tranche la question amont : `BlockOpen` ne porte PAS de condition.
///   Un bloc conditionnel s'exprime en composant un `FlatPageToken::IfBool`
///   à l'intérieur de la plage du bloc (ou en l'englobant), pas en dupliquant
///   la sémantique conditionnelle sur le marqueur de bloc lui-même. Séparation
///   stricte des responsabilités : `BlockOpen`/`BlockEnd` nomment une région
///   de fusion, `IfBool`/`EndIf` conditionnent un rendu — deux axes orthogonaux
///   qui ne doivent pas fusionner dans un seul token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageBlockToken<'src> {
    /// `{% block header %}` — ouverture nommée. `name` est le seul identifiant
    /// de correspondance parent/enfant : la fusion (session ultérieure)
    /// recherchera ce nom dans le template parent pour substituer la plage.
    BlockOpen { name: &'src str },

    /// `{% endblock %}` — fermeture symétrique. Pas de champ `name` : comme
    /// `EndIf`, la FSM de validation associe la fermeture au dernier bloc
    /// ouvert par position, pas par ré-affirmation du nom (moins de surface
    /// d'erreur utilisateur — un `{% endblock wrongname %}` n'existe pas
    /// dans cette forme, donc ne peut pas désynchroniser silencieusement).
    BlockEnd,
}

/// Handle opaque identifiant l'arène d'origine d'une plage de tokens.
///
/// `Copy`, sans lifetime, sans indirection — un `u32` nu. Choix DOD délibéré :
/// l'alternative (porter un `&'ast [FlatPageToken<'src>]` directement dans
/// `NamedBlockRange`) rendrait `ChildTemplateSpec` auto-référentiel vis-à-vis
/// de son propre `Vec<FlatPageToken<'src>>` — struct qui s'emprunte
/// elle-même, incompatible avec la construction en une passe et avec la
/// contrainte `Copy` recherchée pour ce type de handle. `TemplateId` reporte
/// la vérification d'arène sur la valeur elle-même plutôt que sur une
/// relation d'emprunt : une plage sait de quel template elle vient, sans
/// dépendre du contexte d'appel pour rester valide après une copie.
///
/// Assignation : responsabilité du Linker (session ultérieure), qui tiendra
/// l'arène (`Vec<Vec<FlatPageToken<'src>>>` ou équivalent) indexée par cet
/// id. Aucune hypothèse sur la forme de cette arène n'est prise ici — ce
/// type fixe uniquement sa fonction de tag d'origine.
///
/// Vérification : à l'exécution (assert au point de déréférencement dans le
/// Linker), pas à la compilation. Le borrow checker ne peut pas garantir
/// statiquement qu'un `TemplateId` correspond à l'arène qu'on lui présente
/// sans emprunt réel — c'est le tradeoff explicite du choix Index+Tag par
/// rapport à un slice emprunté. Acceptable tant qu'une seule passe de
/// linking traite les templates séquentiellement ; à réévaluer si une passe
/// future traite plusieurs arènes en parallèle dans le même scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TemplateId(pub u32);

/// Correspondance nom-de-bloc → plage de tokens, taguée par arène d'origine.
///
/// `start`/`end` sont des indices dans le `Vec<FlatPageToken<'src>>` identifié
/// par `template`, exclusifs des marqueurs `PageBlockToken` eux-mêmes : la
/// plage couvre le *contenu* du bloc, pas ses délimiteurs. Convention
/// `[start, end)` (`end` exclusif), cohérente avec les conventions de slice
/// Rust — la fusion consommera `&arena[range.template][range.start..range.end]`.
///
/// Le champ `template` élimine par construction la confusion d'arène : une
/// plage extraite d'un enfant A et appliquée par erreur au token-vec d'un
/// enfant B produit un id qui ne correspond pas à l'arène consultée — donc
/// une valeur détectable (assert du Linker), plutôt qu'un résultat
/// silencieusement incohérent (troncature ou contenu halluciné) que
/// produirait un couple `(usize, usize)` nu appliqué au mauvais `Vec`.
///
/// Type de données pur : aucune méthode de validation ou de fusion. La
/// construction de ces plages (parcours de l'AST enfant pour repérer les
/// paires BlockOpen/BlockEnd, assignation du `TemplateId` courant) est un
/// algorithme de la session parseur, pas de celle-ci.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedBlockRange<'src> {
    /// Nom déclaré par `{% block name %}`. Clé de correspondance avec le
    /// parent — deux blocs de même nom dans un même enfant sont une erreur
    /// de linking (`PageLinkError::OrphanBlock` ou variante dédiée future),
    /// pas une responsabilité de ce type.
    pub name: &'src str,
    /// Arène d'origine des indices `start`/`end` ci-dessous.
    pub template: TemplateId,
    /// Index de début de la plage de contenu (inclusif), dans l'AST référencé
    /// par `template`.
    pub start: usize,
    /// Index de fin de la plage de contenu (exclusif), dans l'AST référencé
    /// par `template`.
    pub end: usize,
}

/// Template enfant, forme pré-fusion.
///
/// Porte le chemin `extends` déclaré par `{% extends "parent.marius" %}` et
/// l'ensemble des blocs que l'enfant définit, sous forme de plages dans son
/// propre AST. Ne porte PAS l'AST lui-même (celui-ci vit déjà comme
/// `Vec<FlatPageToken<'src>>`, produit par le futur parseur mode page, non
/// dupliqué ici) — uniquement la métadonnée de structure que la fusion
/// consommera aux côtés de cet AST.
///
/// ─── `extends` comme champ, pas comme token : états illégaux
///     irreprésentables ────────────────────────────────────────────────────
///
///   `extends` est un champ obligatoire de la struct, pas une variante dans
///   un flux de tokens. Conséquence directe : on ne peut pas construire un
///   `ChildTemplateSpec` sans avoir déjà tranché la valeur d'`extends`, et on
///   ne peut pas en construire un qui en porterait deux ou zéro — les deux
///   états qu'un token `Extends` répété ou absent rendrait représentables
///   dans un `Vec<FlatPageToken>` classique. La contrainte "un seul extends,
///   en tête de template" est donc garantie par la forme du type au moment
///   de sa construction, et non vérifiée après coup par une variante
///   d'erreur dédiée (`ExtendsNotFirst` reste nécessaire, mais côté Parser :
///   c'est l'erreur levée en tentant de *construire* ce champ à partir d'un
///   flux qui contredit la grammaire, jamais une invariante à revérifier une
///   fois le type déjà construit). Ce choix explique pourquoi `blocks`, lui,
///   reste un `Vec` : c'est la partie réellement répétable de la grammaire —
///   zéro, un ou N blocs sont tous des états légitimes.
///
/// ─── Rôle dans le futur pipeline de fusion ───────────────────────────────
///
///   La fusion (Normalization, hors périmètre ici) parcourra l'AST du
///   parent, et à chaque `PageBlockToken::BlockOpen { name }` rencontré,
///   cherchera `name` dans `self.blocks`. Si trouvé : substitution de la
///   plage. Si absent : le bloc par défaut du parent est conservé
///   (comportement Jinja2-like standard, à confirmer/trancher explicitement
///   à l'écriture de la fusion — non tranché ici). Un bloc de l'enfant qui
///   ne correspond à aucun `BlockOpen` du parent est orphelin : c'est le
///   rôle prévu de `PageLinkError::OrphanBlock`.
///
/// ─── Allocation ───────────────────────────────────────────────────────────
///
///   `blocks` est un `Vec` : allocation build-time uniquement, comme tous
///   les AST de ce module. Structure jamais exposée au runtime du moteur.
#[derive(Debug, Clone)]
pub struct ChildTemplateSpec<'src> {
    /// Chemin déclaré par `{% extends %}`, tel qu'écrit dans le template —
    /// résolution en chemin manifeste (via `relative_path_for_include_str`,
    /// fonction existante et déjà réutilisable telle quelle) différée à la
    /// session de câblage. Voir doc de tête de struct : la présence et
    /// l'unicité de ce champ ne sont pas revérifiées à l'usage, elles sont
    /// garanties par la construction du type.
    pub extends: &'src str,
    /// Un élément par bloc défini dans cet enfant, dans l'ordre d'apparition
    /// dans l'AST enfant (pas trié par nom : l'ordre d'apparition n'a pas de
    /// signification pour la fusion, mais le préserver évite un tri
    /// superflu tant qu'aucun consommateur n'en a besoin).
    pub blocks: Vec<NamedBlockRange<'src>>,
}

// ─── Erreurs par phase (Action 2) ──────────────────────────────────────────
//
// Une phase ne peut pas retourner une erreur d'un domaine qu'elle ne connaît
// pas — le typage doit refléter le pipeline (Lowering) et non un panier
// d'erreurs partagé. Trois types, un par phase amont du pipeline de
// composition. Nommage : préfixe `Page` conservé (et non les noms nus
// `ParseError`/`LinkError`/`ValidationError`) pour deux raisons — éviter la
// collision de lecture avec `PageParseError` déjà existant et frozen (mode
// fragment, jamais concerné par extends/block), et éviter des noms de type
// pub génériques à un seul mot dans une bibliothèque qui exporte aussi
// `SemanticError`/`ResolverError` sous le même schéma de nommage préfixé. Si
// ce choix de nommage ne convient pas, il se renomme en un remplacement pur
// (aucune des variantes ni leur regroupement par phase n'en dépend).

/// Sortie complète du Parser Mode Page pour **un** fichier (Document 1 §2.2,
/// scaffoldée en Phase 4.6 quand `extends` devient calculable — cf. doc de
/// `parse_page_tokens`).
///
/// ─── Pourquoi `extends` n'est pas une variante de `PageSourceToken` ────────
///
/// Une déclaration `{% extends "path" %}` est une propriété du *fichier*
/// (une seule par fichier, position figée en tête), pas un élément d'un flux
/// homogène de tokens de contenu — la porter comme variante de
/// `PageSourceToken` obligerait tout consommateur de `tokens` (Document 2 :
/// `collect_blocks`, la Validation) à re-vérifier au runtime qu'elle
/// n'apparaît qu'en position 0, alors que ce champ séparé le rend
/// impossible par construction : `tokens` ne contient jamais de déclaration
/// `extends`, point final.
///
/// ─── Pas de `TemplateId` ni de `NamedBlockRange` résolus ───────────────────
///
/// L'assignation d'identité d'arène est une responsabilité de l'admission
/// en arène (Document 2, Phase 5.1) : un fichier parsé isolément n'a pas
/// encore d'arène à laquelle appartenir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPageTemplate<'src> {
    /// Chemin déclaré par `{% extends %}`, si présent en tête de fichier.
    /// `None` : ce fichier est un parent (aucun `extends` rencontré) — un
    /// fichier hors mode page n'atteint jamais cette structure, ce cas est
    /// écarté en amont par `detect_extends` (précondition d'appel, cf. doc
    /// de `parse_page_tokens`).
    pub extends: Option<&'src str>,
    /// Flux de tokens de contenu, dans l'ordre du fichier source. Ne
    /// contient jamais de déclaration `extends` (voir note ci-dessus).
    pub tokens: Vec<PageSourceToken<'src>>,
}

/// Arène de templates Mode Page — donne à chaque fichier admis une identité
/// stable et vérifiable par égalité de valeur (Document 2, §2 ; Phase 5.1).
///
/// ─── Ce que cette phase élimine ─────────────────────────────────────────
///
///   « fichier isolé, sans identité stable ». Un `ParsedPageTemplate<'src>`
///   sorti du Parser (Document 1) n'a pas de position dans une chaîne
///   d'héritage — l'admettre en arène lui attribue un `TemplateId` que les
///   phases suivantes (`NamedBlockRange`, Linker, Lowering — sessions
///   ultérieures) pourront porter sans emprunt auto-référentiel sur le
///   `Vec<ParsedPageTemplate<'src>>` lui-même.
///
/// ─── Ce que cette phase ne touche pas ───────────────────────────────────
///
///   Le contenu des tokens : `admit` ne fait aucune E/S (le fichier est déjà
///   lu et parsé au moment de l'appel) et ne copie aucune donnée — l'arène
///   prend possession du `ParsedPageTemplate<'src>` fourni, qui porte déjà
///   ses propres emprunts sur la source. Aucune logique de blocs
///   (`collect_blocks`) ni de liens (`link`) n'est introduite ici.
///
/// ─── Invariant d'identité ────────────────────────────────────────────────
///
///   `TemplateId` est `Copy`/`Eq` (défini en Phase 3.0, gelé) : deux appels à
///   `admit` produisent deux identifiants distincts (assignation par index
///   de poussée dans `templates`), et `get` sur un identifiant déjà admis
///   retourne exactement le contenu inséré — aucune mutation intermédiaire
///   n'est possible, `templates` n'expose aucune méthode de retrait ou de
///   modification en place.
///
/// ─── Allocation ──────────────────────────────────────────────────────────
///
///   `Vec` de croissance linéaire, une entrée par fichier admis (2 dans le
///   cas courant : enfant + parent — voir Document 2 §6.1 pour le point
///   ouvert sur l'héritage multi-niveaux, non traité par cette structure).
///   Build-time uniquement, jamais exposée au runtime du moteur.
#[derive(Debug, Default)]
pub struct PageArena<'src> {
    templates: Vec<ParsedPageTemplate<'src>>,
}

impl<'src> PageArena<'src> {
    /// Admet un template déjà parsé et lui attribue un `TemplateId` stable.
    ///
    /// Prend possession de `parsed` — aucune copie. L'identifiant retourné
    /// est l'index de poussée dans `templates` : strictement croissant à
    /// chaque appel, jamais réutilisé (pas de méthode de retrait exposée).
    pub fn admit(&mut self, parsed: ParsedPageTemplate<'src>) -> TemplateId {
        let id = TemplateId(self.templates.len() as u32);
        self.templates.push(parsed);
        id
    }

    /// Déréférence un `TemplateId` déjà admis vers son template.
    ///
    /// Précondition : `id` provient d'un appel à `admit` sur cette même
    /// arène. Violation : `panic!` par indexation hors bornes — cohérent
    /// avec la doc de `TemplateId` (vérification à l'exécution, pas de
    /// variante d'erreur dédiée à ce stade squelette ; le Linker, session
    /// ultérieure, décidera s'il faut absorber ce cas en `Result`).
    pub fn get(&self, id: TemplateId) -> &ParsedPageTemplate<'src> {
        &self.templates[id.0 as usize]
    }
}

/// Erreur du Parser Front-end mode page : construction de `PageBlockToken`
/// et `ChildTemplateSpec` à partir du flux de tokens source. Distincte de
/// `PageParseError` (Parser mode fragment, gelé, ne connaît ni `extends` ni
/// `block`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageComposeParseError {
    /// `{% extends %}` rencontré ailleurs qu'en tête de template, ou en
    /// double. Erreur de grammaire, pas de sémantique : sous la grammaire
    /// mode page, `extends` n'est valide qu'en position de tête — une
    /// occurrence supplémentaire ou déplacée ne correspond à aucune
    /// production valide, au même titre qu'un `UnexpectedToken` de
    /// `PageParseError`. Cf. doc de `ChildTemplateSpec::extends`.
    ExtendsNotFirst,
    /// Token reçu ≠ token attendu à cette position de l'automate. Symétrique
    /// de `PageParseError::UnexpectedToken` (Phase 1.3, gelé) — même forme,
    /// domaine d'erreur distinct (Document 1 §0 : `PageComposeParseError` ≠
    /// `PageParseError`, un appelant ne peut pas confondre les deux échecs).
    /// Introduit en Phase 4.3 : nécessaire dès que `parse_page_tokens`
    /// consomme un flux de `RawSpan` structuré, pas seulement pour le
    /// sous-ensemble `Runtime`.
    UnexpectedToken {
        expected: &'static str,
        got: SpanKind,
    },
    /// Itérateur épuisé alors qu'un token était requis pour compléter un
    /// pattern. Symétrique de `PageParseError::UnexpectedEof`.
    UnexpectedEof,
    /// Séquence de bloc non reconnue : `{% if entity.field %}` sans `.` dans
    /// l'ident bloc, ou tout autre motif structurellement invalide pour un
    /// mot-clé par ailleurs traité comme grammaticalement significatif à ce
    /// stade. Symétrique de `PageParseError::InvalidBlockSequence`.
    ///
    /// Portée Phase 4.7 : cette variante couvrait, temporairement (Phases
    /// 4.3 à 4.6), tout mot-clé de bloc encore non représentable par
    /// `PageSourceToken` à ce stade du classifieur. `block` et `endblock`
    /// en sont sortis en Phase 4.4 (branche `Block` dédiée) ; `static` en
    /// est sorti en Phase 4.5 (branche `Static` dédiée) ; `extends` en est
    /// sorti en Phase 4.6 (sa forme structurelle — `Ident(path) BlockClose`
    /// — reste jugée ici, seule sa *position* relève d'`ExtendsNotFirst`,
    /// domaine disjoint, cf. doc de `parse_page_tokens`). Le catch-all
    /// `Unsupported` a clos la grammaire en Phase 4.7 : tout mot-clé de
    /// bloc syntaxiquement bien formé (`Ident … BlockClose`) mais hors
    /// grammaire runtime connue est désormais capturé, jamais rejeté ici.
    /// Cette variante d'erreur n'est pas retirée pour autant : elle reste,
    /// à titre définitif, le domaine des erreurs de grammaire structurelle
    /// des mots-clés déjà reconnus (`if`/`endif`/`block`/`endblock`/
    /// `static`/`extends` — par exemple un `{% if %}` sans point dans
    /// l'identifiant, cf. `split_dotted_page`) **et** celui d'`include`,
    /// explicitement exclu du catch-all `Unsupported` (Phase 4.7, cf. doc
    /// de `parse_page_block`) : structurellement absent de la grammaire
    /// Mode Page, `include` n'est jamais « non supporté », il est rejeté.
    InvalidBlockSequence,
}

/// Erreur du Linker : résolution de dépendances entre templates
/// (`{% extends %}`, `{% static %}`) et correspondance des blocs enfant
/// contre les `BlockOpen` du parent référencé. Connaît l'existence d'autres
/// templates ; le Parser, phase amont, ne le peut pas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageLinkError<'src> {
    /// Chemin déclaré par `{% extends "path" %}` introuvable au moment de la
    /// résolution. Distinct de `ResolverError::IoError` (mode fragment) :
    /// contexte de phase différent (Linker vs résolution de capacité), pas
    /// une duplication.
    ExtendsNotFound { path: &'src str },
    /// `{% block name %}` défini dans un enfant sans `BlockOpen` de même nom
    /// dans l'AST du parent référencé par `extends`. Cf. doc de
    /// `ChildTemplateSpec`.
    OrphanBlock { name: &'src str },
    /// `{% static path %}` référence un fichier introuvable. Distinct
    /// d'`ExtendsNotFound` (chemin de template) et de
    /// `ResolverError::IoError` (mode fragment, `StaticInclude`) : trois
    /// erreurs de fichier manquant, trois contextes syntaxiques et trois
    /// phases différentes — pas de mutualisation prématurée avant d'avoir
    /// écrit les trois call-sites.
    StaticFileNotFound { path: &'src str },
}

/// Erreur de Validation sémantique mode page : propriétés vérifiables sur un
/// unique template déjà syntaxiquement valide, sans connaissance des autres
/// templates. Symétrique de `SemanticError` (mode fragment, Phase 1.4), sur
/// l'axe composition plutôt que sur l'axe condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageValidationError<'src> {
    /// Condition de `{% if %}` (mode page) qui ne référence pas un champ
    /// u8-sentinelle attendu par le contrat acté ci-dessus (Bool vs u8).
    /// Distincte d'une erreur Parser : le champ existe et est bien formé
    /// syntaxiquement, seul son type ne satisfait pas le contrat `Pod` du
    /// moteur — erreur sémantique, pas structurelle.
    NonBoolIfCondition { entity: &'src str, field: &'src str },
    /// `{% for %}` rencontré dans un template mode page. La spécification
    /// v1.1 exclut les boucles du modèle de rendu déterministe AOT (capacité
    /// non bornée statiquement) — variante nommée plutôt qu'un rejet
    /// générique qui masquerait la raison précise.
    ForLoopDetected,
    /// Mot-clé de requête relationnelle (jointure, filtre, tri…) détecté
    /// dans un template. Le mode page reste un langage de présentation :
    /// toute logique relationnelle appartient à la couche SQL/schema.
    RelationalKeyword { keyword: &'src str },
    /// `{% block %}` imbriqué dans un autre `{% block %}`. Symétrique de
    /// `SemanticError::NestedIfNotSupported` : même contrainte de platitude,
    /// appliquée à l'axe "bloc de fusion" plutôt qu'à l'axe "condition".
    /// Vérifiable sur l'AST d'un seul template, sans résolution externe —
    /// c'est pourquoi cette variante est Validation et non Link, malgré son
    /// lien thématique avec `PageBlockToken`.
    NestedBlock { name: &'src str },
}

/// Référence à une inclusion statique déduplique-able : futur `{% static %}`
/// du mode page.
///
/// ─── Contrat, distinct de `FlatPageToken::StaticInclude` ─────────────────
///
///   `StaticInclude` (mode fragment, existant, inchangé) : chaque occurrence
///   dans l'AST porte son propre `len`, mesuré et sommé indépendamment par
///   `resolve_and_measure`. Un fichier inclus à N endroits coûte N × len()
///   dans `TemplateMetrics::total_static_bytes` — correct pour le mode
///   fragment, où deux inclusions du même chemin sont deux `include_str!`
///   distincts sans lien entre eux.
///
///   `StaticPartialRef` (mode page, ce type) : le fichier référencé est
///   matérialisé une seule fois comme constante partagée
///   `static_partials::{IDENT}` (voir `static_const_ident`, fonction
///   existante, réutilisable sans modification pour calculer `IDENT` depuis
///   le chemin). Le coût en octets de ce fichier doit être compté UNE fois
///   dans les métriques globales, quel que soit le nombre d'occurrences de
///   `StaticPartialRef` qui le référencent dans l'AST fusionné parent+enfant.
///
///   Conséquence directe sur le futur calcul de capacité : la somme ne peut
///   pas être `Σ occurrences.len()` (ce que fait `resolve_and_measure` pour
///   `StaticInclude`) — elle doit être `Σ chemins_uniques.len()`. C'est
///   pourquoi ce type ne porte délibérément PAS de champ `len` par
///   occurrence : le coder ici inviterait à sommer par occurrence par
///   symétrie avec `StaticInclude`, exactement l'erreur que ce type doit
///   rendre impossible par construction. La taille du fichier sera portée
///   par un registre séparé, keyé par `const_ident`, écrit dans la session
///   qui résoudra `{% static %}` — hors périmètre ici.
///
/// ─── Pourquoi une variante distincte de `StaticInclude` plutôt qu'une
///     réutilisation avec déduplication ajoutée dans le résolveur ──────────
///
///   Réutiliser `StaticInclude` et déduplicer au niveau de
///   `resolve_and_measure` masquerait la distinction dans le type : rien
///   n'empêcherait alors un futur appelant de construire un `StaticInclude`
///   pour du contenu partagé et un autre pour du contenu non partagé, sans
///   qu'aucune signature ne le distingue. En portant la distinction dans le
///   type (`StaticPartialRef` ≠ `StaticInclude`), le compilateur rend la
///   confusion impossible : un `match` exhaustif sur le futur enum englobant
///   devra traiter les deux cas séparément, ce qui est le point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticPartialRef<'src> {
    /// Chemin tel qu'écrit dans le template, avant résolution manifeste.
    /// Miroir de `StaticInclude::original_path` — même convention, même
    /// lifetime, pour que la session de câblage puisse réutiliser
    /// `relative_path_for_include_str` sans adaptation. Non quoté (Phase
    /// 4.5, cf. doc `PageSourceToken::Static`) : slice brute issue du
    /// scanner, identique en forme au `path` d'`{% include %}`.
    pub original_path: &'src str,
}

#[cfg(test)]
mod tests_phase_3_0_page_mode_types {
    use super::{
        ChildTemplateSpec, NamedBlockRange, PageBlockToken, PageComposeParseError, PageLinkError,
        PageValidationError, StaticPartialRef, TemplateId,
    };

    /// Jalon Vert — les nouveaux types sont Copy/Clone/PartialEq comme leurs
    /// équivalents mode fragment (FlatPageToken l'est déjà pour IfBool/EndIf).
    /// Preuve par réaffectation sans move, comme `all_variants_are_copy`
    /// (Phase 1.1) — même style de preuve, cohérence de méthode.
    #[test]
    fn page_block_token_is_copy() {
        let a = PageBlockToken::BlockOpen { name: "header" };
        let _b = a;
        let _c = a;
        assert_eq!(a, PageBlockToken::BlockOpen { name: "header" });
    }

    /// Jalon Vert — le handle `TemplateId` distingue deux plages
    /// structurellement identiques (mêmes `start`/`end`) mais d'arènes
    /// différentes : c'est exactement l'invariant que ce type doit rendre
    /// vérifiable, ici prouvé par simple inégalité de valeur.
    #[test]
    fn named_block_range_is_copy_half_open_and_arena_tagged() {
        let child_a = TemplateId(0);
        let child_b = TemplateId(1);
        let r_a = NamedBlockRange {
            name: "header",
            template: child_a,
            start: 3,
            end: 7,
        };
        let r_b = NamedBlockRange {
            name: "header",
            template: child_b,
            start: 3,
            end: 7,
        };
        let _copy = r_a; // Copy, pas de move

        assert_eq!(
            r_a.end - r_a.start,
            4,
            "plage [start, end) : 4 tokens couverts"
        );
        assert_ne!(
            r_a, r_b,
            "même range, arène différente : distinguable par valeur"
        );
    }

    /// Jalon Vert — ChildTemplateSpec ne porte que de la métadonnée, jamais
    /// l'AST lui-même : ce test construit la forme attendue sans dépendance
    /// à un futur parseur mode page (zéro couplage avec le pipeline gelé).
    #[test]
    fn child_template_spec_shape() {
        let this_child = TemplateId(0);
        let spec = ChildTemplateSpec {
            extends: "layouts/base.marius",
            blocks: vec![
                NamedBlockRange {
                    name: "header",
                    template: this_child,
                    start: 0,
                    end: 2,
                },
                NamedBlockRange {
                    name: "body",
                    template: this_child,
                    start: 3,
                    end: 9,
                },
            ],
        };
        assert_eq!(spec.blocks.len(), 2);
        assert_eq!(spec.blocks[0].name, "header");
        assert!(spec.blocks.iter().all(|b| b.template == this_child));
    }

    /// Jalon Vert — StaticPartialRef ne porte pas de `len` (contrairement à
    /// StaticInclude) : ce test documente l'absence de champ par sa forme,
    /// pas par une assertion runtime (le compilateur est la preuve).
    #[test]
    fn static_partial_ref_has_no_len_field() {
        let r = StaticPartialRef {
            original_path: "partials/nav.html",
        };
        assert_eq!(r.original_path, "partials/nav.html");
    }

    /// Jalon Vert — les trois erreurs de phase sont des types distincts :
    /// une fonction de signature `Result<_, PageLinkError>` ne peut
    /// physiquement pas retourner `PageComposeParseError::ExtendsNotFirst`.
    /// Preuve par construction indépendante, pas par introspection runtime —
    /// l'invariant recherché est justement de ne pas être testable au sens
    /// classique : il est vérifié par le compilateur à chaque call-site
    /// futur, pas ici.
    #[test]
    fn phase_errors_are_distinct_types() {
        let _parse: PageComposeParseError = PageComposeParseError::ExtendsNotFirst;
        let _link: PageLinkError<'_> = PageLinkError::OrphanBlock { name: "sidebar" };
        let _validation: PageValidationError<'_> = PageValidationError::ForLoopDetected;
    }
}

#[cfg(test)]
mod tests_phase_5_1_page_arena {
    use super::{PageArena, ParsedPageTemplate};

    /// Jalon Vert (roadmap §5.1) — deux admissions successives produisent
    /// deux `TemplateId` distincts : l'identité est assignée par position
    /// de poussée, jamais réutilisée ni partagée entre deux fichiers admis.
    #[test]
    fn admit_twice_yields_distinct_template_ids() {
        let mut arena = PageArena::default();
        let child = ParsedPageTemplate {
            extends: Some("parent.marius"),
            tokens: Vec::new(),
        };
        let parent = ParsedPageTemplate {
            extends: None,
            tokens: Vec::new(),
        };

        let child_id = arena.admit(child);
        let parent_id = arena.admit(parent);

        assert_ne!(
            child_id, parent_id,
            "deux admissions distinctes doivent produire deux TemplateId distincts"
        );
    }

    /// Jalon Vert (roadmap §5.1) — `get` après `admit` retourne le contenu
    /// inséré inchangé (égalité de valeur complète, pas seulement des
    /// tokens) : l'arène ne copie ni ne transforme rien à l'admission.
    #[test]
    fn get_after_admit_returns_unchanged_content() {
        let mut arena = PageArena::default();
        let original = ParsedPageTemplate {
            extends: Some("parent.marius"),
            tokens: Vec::new(),
        };
        let expected = original.clone();

        let id = arena.admit(original);

        assert_eq!(
            arena.get(id),
            &expected,
            "le contenu retourné par get doit être identique à celui admis"
        );
    }
}
