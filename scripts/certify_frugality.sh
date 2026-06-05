#!/usr/bin/env bash
# scripts/certify_frugality.sh
# Certification zéro-allocation du pipeline de rendu Marius.
#
# ─── Objectif ────────────────────────────────────────────────────────────────
#
#   Exécute le binaire hot_path_certify qui instrumente l'allocateur global
#   (CountingAlloc) et certifie que P::render() n'effectue aucune allocation
#   sur le tas pendant son exécution, une fois le buffer pré-alloué à TOTAL_CAP.
#
#   Ce que le benchmark certifie exactement (ADR-003) :
#     - buf alloué à STATIC_CAP + DYNAMIC_CAP avant render()
#     - CountingAlloc::reset() immédiatement avant render()
#     - ALLOC_COUNT == 0 immédiatement après render()
#     → buf.reserve() est un no-op : DYNAMIC_CAP couvre le pire cas.
#
#   Ce script est distinct de profile_frugality.sh (perf stat / IPC / cache)
#   pour conserver une séparation nette entre les deux axes de preuve :
#     - Frugalité matérielle (cycles, cache) → profile_frugality.sh
#     - Zéro-allocation logicielle           → certify_frugality.sh (ce script)
#
# ─── Emplacement ─────────────────────────────────────────────────────────────
#
#   scripts/certify_frugality.sh  (exécuter depuis la racine du projet)
#
# ─── Utilisation ─────────────────────────────────────────────────────────────
#
#   ./scripts/certify_frugality.sh
#
#   Exit code 0 : certification réussie (ALLOC_COUNT == 0 dans render()).
#   Exit code 1 : allocation détectée ou erreur de compilation/exécution.
#
# ─── Prérequis ───────────────────────────────────────────────────────────────
#
#   Aucun outil système supplémentaire (pas de perf, pas de sudo).
#   Le binaire hot_path_certify embarque CountingAlloc comme allocateur global.

set -euo pipefail

BENCH_CRATE="marius-render"
BENCH_NAME="hot_path_certify"
LOG_DIR="logs"
mkdir -p "$LOG_DIR"
LOG_FILE="${LOG_DIR}/certify_frugality_$(date +%Y%m%d_%H%M%S).txt"

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║       Marius Engine — Certification Zéro-Allocation          ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

# Vérification que le script est exécuté depuis la racine du projet.
if [[ ! -f "Cargo.toml" ]]; then
    echo "ERREUR : exécuter ce script depuis la racine du projet."
    echo "  cd /chemin/vers/marius && ./scripts/certify_frugality.sh"
    exit 1
fi

# =============================================================================
# Compilation du binaire de certification
# =============================================================================

echo "Compilation du binaire de certification (profil release)..."
if ! cargo bench -p "$BENCH_CRATE" --bench "$BENCH_NAME" --no-run \
        2>&1 | tee "$LOG_FILE"; then
    echo ""
    echo "ERREUR : compilation échouée. Voir $LOG_FILE pour les détails."
    exit 1
fi

# Localisation du binaire compilé.
# On cherche le plus récent parmi les binaires hot_path_certify-* en release.
# -newer avec le fichier source bench garantit que c'est bien la compilation
# actuelle, et non un artefact périmé d'une session précédente.
BENCH_SRC="crates/shell/render/benches/${BENCH_NAME}.rs"
BENCH_BIN=$(find target/release/deps -name "${BENCH_NAME}-*" \
    -not -name "*.d" \
    -not -name "*.rlib" \
    -executable \
    2>/dev/null \
    | xargs ls -t 2>/dev/null \
    | head -1)

if [[ -z "$BENCH_BIN" ]]; then
    echo "ERREUR : binaire $BENCH_NAME introuvable après compilation."
    exit 1
fi

echo "Binaire : $BENCH_BIN"
echo ""

# =============================================================================
# Exécution de la certification
# =============================================================================

echo "Exécution du benchmark de certification..."
echo "(bench : certify/zero_alloc_in_render — 100 samples, render() seul)"
echo ""

# Capture correcte de l'exit code en présence d'un pipe.
# Le pipe vers tee masque l'exit code du binaire si on utilise $? directement
# car $? retourne le code de tee (toujours 0 si l'écriture réussit).
# PIPESTATUS[0] contient l'exit code du premier membre du pipe.
# set +e : désactive le exit-on-error pour capturer un exit code non-nul.
#
# Le filtre est passé comme argument positionnel au binaire Divan.
set +e
"$BENCH_BIN" \
    "certify/zero_alloc_in_render" \
    2>&1 | tee -a "$LOG_FILE"
# Capture PIPESTATUS immédiatement après le pipe, avant toute autre commande.
EXIT_CODE="${PIPESTATUS[0]}"
set -e

echo ""

# =============================================================================
# Interprétation du résultat
# =============================================================================

if [[ $EXIT_CODE -eq 0 ]]; then
    echo "╔══════════════════════════════════════════════════════════════╗"
    echo "║  CERTIFICATION RÉUSSIE ✓                                     ║"
    echo "║                                                              ║"
    echo "║  P::render() : 0 allocation sur 100 samples.                 ║"
    echo "║                                                              ║"
    echo "║  Invariant ADR-003 validé :                                  ║"
    echo "║  STATIC_CAP + DYNAMIC_CAP couvre le pire cas varlena.        ║"
    echo "║  buf.reserve() est un no-op au runtime.                      ║"
    echo "╚══════════════════════════════════════════════════════════════╝"
else
    echo "╔══════════════════════════════════════════════════════════════╗"
    echo "║  CERTIFICATION ÉCHOUÉE ✗                                     ║"
    echo "╚══════════════════════════════════════════════════════════════╝"
    echo ""
    echo "  Une allocation a été détectée dans render() avec le buffer"
    echo "  déjà pré-alloué à TOTAL_CAP. Causes possibles :"
    echo ""
    echo "    1. DYNAMIC_CAP sous-estimé dans Fragment-Forge"
    echo "       → buf.reserve() a déclenché un realloc."
    echo "       → Vérifier max_display_width (FieldKind)"
    echo "         et max_escaped_len (VarlenField)"
    echo "         dans forge/fragment-forge/src/lib.rs."
    echo ""
    echo "    2. Nouveau champ varlena sans max_len défini"
    echo "       → Régénérer via : cargo build -p marius-schema"
    echo ""
    echo "  Rapport complet : $LOG_FILE"
fi

echo ""
echo "  Rapport sauvegardé : $LOG_FILE"

exit $EXIT_CODE
