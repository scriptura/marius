//! Phase 1.2 — Scanner lexical isolé : `&'src str` → sous-slices `RawSpan`.
//! Zéro allocation heap, zéro sémantique résolue (pas de distinction
//! keyword vs ident, pas de lookup de schéma).

// =============================================================================
// Phase 1.2 — Scanner Lexical Isolé
// =============================================================================
//
// Responsabilité unique : découper `&'src str` en sous-slices typées (RawSpan).
//
// Invariants stricts :
//   - Zéro allocation heap. Aucun Vec, String, Box dans le corps du scanner.
//   - Tous les RawSpan::slice pointent directement dans `src` (fat pointer).
//   - `Scanner::pos` est toujours sur une frontière de char UTF-8 valide.
//     → Garanti en Literal (find() retourne des offsets de frontières).
//     → Garanti en InExpr (seuls des bytes ASCII sont consommés : ident, `.`, `{{`, `}}`).
//     → Garanti en InBlock sous l'hypothèse ASCII (keywords, paths, identifiants SQL).
//       Un byte non-ASCII en InBlock interrompt le token ; Phase 1.4 remonte l'erreur.
//   - Aucune sémantique résolue ici : pas de distinction keyword vs ident, pas de lookup.

/// Catégorie syntaxique brute d'un span issu du scanner.
///
/// `Punct` est émis uniquement en mode `InExpr` pour le séparateur `.`.
/// En mode `InBlock`, `entity.field` est émis en un seul `Ident` — Phase 1.3
/// se charge de la découpe sur `.`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Literal,    // Texte HTML verbatim hors délimiteurs
    ExprOpen,   // `{{`
    ExprClose,  // `}}`
    BlockOpen,  // `{%`
    BlockClose, // `%}`
    Ident,      // Identifiant (entity, field, keyword, chemin de fichier)
    Punct,      // `.` — séparateur entity.field dans {{ … }} uniquement
}

/// Sous-slice typée pointant directement dans la source brute du template.
///
/// `'src` est lié à la durée de vie de la `String` lue par `fs::read_to_string`
/// dans la fonction mère de `build.rs`. Le span ne survit jamais à cette portée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawSpan<'src> {
    pub slice: &'src str,
    pub kind: SpanKind,
}

// ─── État interne ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Literal, // Entre les blocs : tout est HTML statique jusqu'au prochain délimiteur.
    InExpr,  // Intérieur de `{{ … }}` : Ident, Punct, ExprClose.
    InBlock, // Intérieur de `{% … %}` : Ident (token brut), BlockClose.
}

struct Scanner<'src> {
    src: &'src str,
    pos: usize, // Offset byte courant — toujours sur une frontière char valide.
    mode: Mode,
}

impl<'src> Scanner<'src> {
    fn new(src: &'src str) -> Self {
        Self {
            src,
            pos: 0,
            mode: Mode::Literal,
        }
    }

    /// Avance `pos` au-delà des espaces ASCII (U+0009, U+000A, U+000D, U+0020).
    ///
    /// Tous ces bytes sont des chars ASCII single-byte : `pos` reste sur une
    /// frontière valide après l'appel.
    #[inline]
    fn skip_ws(&mut self) {
        let b = self.src.as_bytes();
        while self.pos < self.src.len() {
            match b[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }
}

impl<'src> Iterator for Scanner<'src> {
    type Item = RawSpan<'src>;

    fn next(&mut self) -> Option<Self::Item> {
        let src = self.src;

        if self.pos >= src.len() {
            return None;
        }

        match self.mode {
            // ─── Literal ───────────────────────────────────────────────────
            // Cherche le prochain `{{` ou `{%`.
            // Émet le Literal précédant le délimiteur, puis le délimiteur lui-même
            // (en deux appels distincts — pas de buffer intermédiaire).
            Mode::Literal => {
                let rest = &src[self.pos..];

                // `str::find` retourne des offsets sur des frontières char valides.
                let rel = match (rest.find("{{"), rest.find("{%")) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (Some(a), None) | (None, Some(a)) => Some(a),
                    (None, None) => None,
                };

                match rel {
                    // Pas de délimiteur : le reste est un unique Literal.
                    None => {
                        let span = RawSpan {
                            slice: &src[self.pos..],
                            kind: SpanKind::Literal,
                        };
                        self.pos = src.len();
                        Some(span)
                    }
                    // Délimiteur immédiat : l'émettre et basculer de mode.
                    Some(0) => {
                        let p = self.pos;
                        if src[p..].starts_with("{{") {
                            self.mode = Mode::InExpr;
                            self.pos = p + 2;
                            Some(RawSpan {
                                slice: &src[p..p + 2],
                                kind: SpanKind::ExprOpen,
                            })
                        } else {
                            // starts_with("{%") — seule autre option possible
                            self.mode = Mode::InBlock;
                            self.pos = p + 2;
                            Some(RawSpan {
                                slice: &src[p..p + 2],
                                kind: SpanKind::BlockOpen,
                            })
                        }
                    }
                    // Literal précède le délimiteur.
                    // On émet le Literal et on reste en mode Literal :
                    // le délimiteur sera émis au prochain appel.
                    Some(rel) => {
                        let end = self.pos + rel;
                        let span = RawSpan {
                            slice: &src[self.pos..end],
                            kind: SpanKind::Literal,
                        };
                        self.pos = end;
                        Some(span)
                    }
                }
            }

            // ─── InExpr ────────────────────────────────────────────────────
            // Produit : Ident ([a-zA-Z0-9_]+), Punct (`.`), ExprClose (`}}`).
            // Les espaces sont ignorés (skip_ws).
            // Un `{{` non fermé retourne None ; Phase 1.4 détecte le déséquilibre.
            Mode::InExpr => {
                self.skip_ws();
                if self.pos >= src.len() {
                    return None; // `{{` non fermé — invalide, catchable en Phase 1.4
                }

                let p = self.pos;

                if src[p..].starts_with("}}") {
                    self.mode = Mode::Literal;
                    self.pos = p + 2;
                    return Some(RawSpan {
                        slice: &src[p..p + 2],
                        kind: SpanKind::ExprClose,
                    });
                }

                if src[p..].starts_with('.') {
                    self.pos = p + 1;
                    return Some(RawSpan {
                        slice: &src[p..p + 1],
                        kind: SpanKind::Punct,
                    });
                }

                // Identifiant : séquence de bytes ASCII alphanumériques ou `_`.
                // Tous single-byte → `pos` reste sur une frontière valide.
                let start = p;
                let b = src.as_bytes();
                while self.pos < src.len()
                    && (b[self.pos].is_ascii_alphanumeric() || b[self.pos] == b'_')
                {
                    self.pos += 1;
                }

                if self.pos > start {
                    Some(RawSpan {
                        slice: &src[start..self.pos],
                        kind: SpanKind::Ident,
                    })
                } else {
                    // Byte inattendu (non-ASCII ou ponctuation inconnue).
                    // Avance d'un byte et relance : la récursion est bornée à 1 niveau
                    // car le byte suivant sera soit un char reconnu, soit `}}`.
                    self.pos += 1;
                    self.next()
                }
            }

            // ─── InBlock ───────────────────────────────────────────────────
            // Produit : Ident (token brut), BlockClose (`%}`).
            // Un token = séquence contiguë non-blanc non-`%}`.
            // Inclut les chemins (`dir/file.html`) et `entity.field` sans découpage.
            // Hypothèse : tout contenu de bloc est ASCII (identifiants SQL, paths, keywords).
            Mode::InBlock => {
                self.skip_ws();
                if self.pos >= src.len() {
                    return None; // `{%` non fermé — invalide, catchable en Phase 1.4
                }

                let p = self.pos;
                let b = src.as_bytes();

                if b[p] == b'%' && p + 1 < src.len() && b[p + 1] == b'}' {
                    self.mode = Mode::Literal;
                    self.pos = p + 2;
                    return Some(RawSpan {
                        slice: &src[p..p + 2],
                        kind: SpanKind::BlockClose,
                    });
                }

                // Scan byte par byte jusqu'à un espace ou `%}`.
                // Chaque byte consommé est ASCII (hypothèse documentée ci-dessus) :
                // `pos` reste sur une frontière char valide.
                let start = p;
                while self.pos < src.len() {
                    let byte = b[self.pos];
                    if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                        break;
                    }
                    // Détection de `%}` sur deux bytes ASCII consécutifs.
                    if byte == b'%' && self.pos + 1 < src.len() && b[self.pos + 1] == b'}' {
                        break;
                    }
                    self.pos += 1;
                }

                if self.pos > start {
                    Some(RawSpan {
                        slice: &src[start..self.pos],
                        kind: SpanKind::Ident,
                    })
                } else {
                    None // Byte non-ASCII isolé — Phase 1.4 remonte l'erreur structurelle.
                }
            }
        }
    }
}

/// Retourne un itérateur de `RawSpan<'src>` sur la source brute du template.
///
/// L'itérateur est alloué sur la pile (24 octets : `&str` fat pointer + `usize` + `Mode`).
/// Zéro allocation heap dans le corps du scanner.
pub fn scan(src: &str) -> impl Iterator<Item = RawSpan<'_>> {
    Scanner::new(src)
}

// =============================================================================
// Tests — Phase 1.2
// =============================================================================

#[cfg(test)]
mod tests_phase_1_2 {
    use super::{RawSpan, SpanKind, scan};

    // Helper : construit un RawSpan depuis une &'static str.
    // PartialEq sur &str compare le contenu, pas l'adresse.
    // Un RawSpan issu du scanner (slice dans `src`) égale un RawSpan construit
    // depuis un littéral statique si leurs contenus sont identiques.
    fn s(slice: &str, kind: SpanKind) -> RawSpan<'_> {
        RawSpan { slice, kind }
    }

    /// Jalon Vert Phase 1.2 — séquence exacte pour `"hello {{ user.name }} world"`.
    ///
    /// Vérifie : nombre, ordre, contenu textuel et catégorie de chaque span.
    /// Pas d'assertion sur les adresses mémoire (la découpe par contenu suffit).
    #[test]
    fn scan_expr_interpolation() {
        let src = "hello {{ user.name }} world";
        let got: Vec<_> = scan(src).collect();

        let expected = [
            s("hello ", SpanKind::Literal),
            s("{{", SpanKind::ExprOpen),
            s("user", SpanKind::Ident),
            s(".", SpanKind::Punct),
            s("name", SpanKind::Ident),
            s("}}", SpanKind::ExprClose),
            s(" world", SpanKind::Literal),
        ];

        assert_eq!(got.len(), expected.len(), "nombre de spans incorrect");
        for (i, (got_span, exp)) in got.iter().zip(expected.iter()).enumerate() {
            assert_eq!(got_span, exp, "span[{i}] incorrect");
        }
    }

    /// Bloc conditionnel complet avec Literal intercalé.
    /// Vérifie que InBlock scanne `entity.field` comme un seul Ident
    /// (la découpe sur `.` appartient à Phase 1.3).
    #[test]
    fn scan_block_if_endif() {
        let src = "{% if user.active %}oui{% endif %}";
        let got: Vec<_> = scan(src).collect();

        let expected = [
            s("{%", SpanKind::BlockOpen),
            s("if", SpanKind::Ident),
            s("user.active", SpanKind::Ident),
            s("%}", SpanKind::BlockClose),
            s("oui", SpanKind::Literal),
            s("{%", SpanKind::BlockOpen),
            s("endif", SpanKind::Ident),
            s("%}", SpanKind::BlockClose),
        ];

        assert_eq!(got.len(), expected.len(), "nombre de spans incorrect");
        for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
            assert_eq!(g, e, "span[{i}]");
        }
    }

    /// Cas limites : source vide et source sans délimiteur.
    #[test]
    fn scan_empty_and_literal_only() {
        assert!(
            scan("").next().is_none(),
            "source vide doit être épuisée immédiatement"
        );

        let got: Vec<_> = scan("<p>texte statique</p>").collect();
        assert_eq!(got, [s("<p>texte statique</p>", SpanKind::Literal)]);
    }

    /// Vérifie qu'un délimiteur en tête de source produit ExprOpen sans Literal vide.
    #[test]
    fn scan_delimiter_at_start() {
        let got: Vec<_> = scan("{{ x }}").collect();
        assert_eq!(
            got[0].kind,
            SpanKind::ExprOpen,
            "le premier span doit être ExprOpen, pas un Literal vide"
        );
    }
}
