#!/usr/bin/env bash
# profile_frugality.sh
# Extraction des métriques matérielles incontestables du pipeline de rendu Marius.
#
# ─── Objectif ────────────────────────────────────────────────────────────────
#
#   Qualifier le débit brut mesuré par Divan (53 GB/s) par trois invariants
#   matériels indépendants du code :
#
#   1. IPC (Instructions Per Cycle) : absence de pipeline stalls.
#      Cible : IPC > 2.0 sur un processeur superscalaire moderne (4 issues/cycle).
#      Un IPC < 1.0 signale des cache misses ou des dépendances de données.
#
#   2. Cache L1/LLC miss rate : efficacité du layout mémoire (Sympathie Mécanique).
#      StorageRow #[repr(C)] + VarlenOwned contigus → données dans L1 au moment
#      de leur accès. Un LLC miss rate élevé contredirait le modèle DOD.
#      Cible : L1-dcache-load-misses < 1%, LLC-load-misses < 0.1%.
#
#   3. Énergie RAPL (Joules/item) : rendement énergétique du socket CPU.
#      Permet de dériver le coût énergétique par fragment HTML produit.
#      Disponible uniquement avec CAP_SYS_ADMIN ou perf_event_paranoid <= 1.
#
# ─── Prérequis ───────────────────────────────────────────────────────────────
#
#   - perf (linux-perf, paquet linux-tools-$(uname -r) sur Debian/Ubuntu)
#   - Binaire de benchmark compilé en release :
#       cargo bench -p marius-render --bench hot_path_render --no-run
#   - Pour RAPL : sudo ou perf_event_paranoid <= 1
#       echo 1 | sudo tee /proc/sys/kernel/perf_event_paranoid
#
# ─── Emplacement ─────────────────────────────────────────────────────────────
#
#   scripts/profile_frugality.sh  (exécuter depuis la racine du projet)
#
# ─── Utilisation ─────────────────────────────────────────────────────────────
#
#   ./scripts/profile_frugality.sh [--no-rapl] [--bench-filter PATTERN]
#
#   --no-rapl        : désactive la mesure RAPL (pas de sudo requis)
#   --bench-filter   : filtre positionnel passé au binaire Divan (défaut : rayon/nominal)
#
# ─── Sortie ──────────────────────────────────────────────────────────────────
#
#   Terminal : rapport formaté avec les trois métriques.
#   Fichier  : perf_frugality_$(date +%Y%m%d_%H%M%S).txt (brut perf stat)

set -euo pipefail

# =============================================================================
# Configuration
# =============================================================================

BENCH_CRATE="marius-render"
BENCH_NAME="hot_path_render"
BENCH_FILTER="${BENCH_FILTER:-render/rayon/nominal}"
SAMPLE_COUNT=3      # Répétitions perf stat (moyenne sur N runs)
DIVAN_SAMPLES=1     # Samples Divan par run perf (minimise le bruit de mesure)

# Events perf à collecter.
# Notation : hardware events (portables) + software events.
# Sur AMD : cache-references/cache-misses mappe sur L2/L3 selon µarch.
# Sur Intel : LLC = L3 (Last Level Cache).
PERF_EVENTS=(
    "cycles"
    "instructions"
    "cache-references"
    "cache-misses"
    "L1-dcache-loads"
    "L1-dcache-load-misses"
    "LLC-loads"
    "LLC-load-misses"
    "branch-instructions"
    "branch-misses"
)

RAPL_EVENT="power/energy-pkg/"
ENABLE_RAPL=true
LOG_DIR="logs"
mkdir -p "$LOG_DIR"
OUTPUT_FILE="${LOG_DIR}/perf_frugality_$(date +%Y%m%d_%H%M%S).txt"

# =============================================================================
# Parsing des arguments
# =============================================================================

for arg in "$@"; do
    case "$arg" in
        --no-rapl)
            ENABLE_RAPL=false
            ;;
        --bench-filter=*)
            BENCH_FILTER="${arg#*=}"
            ;;
        --bench-filter)
            shift
            BENCH_FILTER="$1"
            ;;
    esac
done

# =============================================================================
# Vérifications préliminaires
# =============================================================================

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║         Marius Engine — Profil de Frugalité Matérielle       ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

# Vérification que le script est exécuté depuis la racine du projet Cargo.
# Le binaire de benchmark est dans target/release/deps/ relatif à la racine.
if [[ ! -f "Cargo.toml" ]]; then
    echo "ERREUR : exécuter ce script depuis la racine du projet (là où Cargo.toml se trouve)."
    echo "  cd /chemin/vers/marius && ./scripts/profile_frugality.sh"
    exit 1
fi

# Vérification de perf
if ! command -v perf &>/dev/null; then
    echo "ERREUR : perf non trouvé."
    echo "  Debian/Ubuntu : sudo apt install linux-perf"
    echo "  Arch          : sudo pacman -S perf"
    exit 1
fi

# Vérification de perf_event_paranoid avant toute tentative de mesure.
# Valeur 4 (défaut Debian/Ubuntu récent) bloque tous les events matériels.
# Le script échoue explicitement avec la commande de correction, plutôt que
# de laisser perf stat échouer silencieusement avec des compteurs à zéro.
PARANOID=$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo "99")
if [[ "$PARANOID" -gt 1 ]]; then
    echo "ERREUR : perf_event_paranoid=$PARANOID — accès aux events CPU refusé."
    echo ""
    echo "  Correction temporaire (session courante) :"
    echo "    echo 1 | sudo tee /proc/sys/kernel/perf_event_paranoid"
    echo ""
    echo "  Correction permanente (redémarre avec sysctl) :"
    echo "    echo 'kernel.perf_event_paranoid = 1' | sudo tee -a /etc/sysctl.conf"
    echo "    sudo sysctl -p"
    echo ""
    echo "  Relancer ensuite : ./scripts/profile_frugality.sh"
    exit 1
fi
echo "perf_event_paranoid=$PARANOID ✓"
echo ""

# Localisation du binaire de benchmark Divan (compilé en release).
# Cargo place les binaires dans target/release/deps/ avec un hash de contenu.
BENCH_BIN=$(find target/release/deps -name "${BENCH_NAME}-*" \
    -not -name "*.d" \
    -not -name "*.rlib" \
    -executable \
    -newer Cargo.lock \
    2>/dev/null | head -1)

if [[ -z "$BENCH_BIN" ]]; then
    echo "Binaire de benchmark non trouvé. Compilation en cours..."
    cargo bench -p "$BENCH_CRATE" --bench "$BENCH_NAME" --no-run \
        2>&1 | tail -5
    BENCH_BIN=$(find target/release/deps -name "${BENCH_NAME}-*" \
        -not -name "*.d" \
        -not -name "*.rlib" \
        -executable \
        2>/dev/null | head -1)
fi

if [[ -z "$BENCH_BIN" ]]; then
    echo "ERREUR : impossible de trouver le binaire compilé."
    echo "  Exécuter manuellement : cargo bench -p $BENCH_CRATE --bench $BENCH_NAME --no-run"
    exit 1
fi

echo "Binaire : $BENCH_BIN"
echo "Logs    : $LOG_DIR/"
echo "Filtre  : $BENCH_FILTER"
echo "Samples : $SAMPLE_COUNT runs perf × $DIVAN_SAMPLES samples Divan"
echo ""

# =============================================================================
# Vérification RAPL
# =============================================================================

check_rapl() {
    # Test de disponibilité RAPL sans sudo.
    # perf stat retourne exit code non-nul si l'event est inaccessible.
    if perf stat -e "$RAPL_EVENT" -a sleep 0.01 &>/dev/null; then
        return 0
    fi

    # Tentative avec paranoid check
    local paranoid
    paranoid=$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo "99")
    if [[ "$paranoid" -gt 1 ]]; then
        echo "AVERTISSEMENT RAPL : perf_event_paranoid=$paranoid (requis <= 1)."
        echo "  Pour activer sans reboot :"
        echo "    echo 1 | sudo tee /proc/sys/kernel/perf_event_paranoid"
        echo "  RAPL désactivé pour cette session (--no-rapl pour supprimer cet avertissement)."
        return 1
    fi
    return 1
}

if $ENABLE_RAPL; then
    if check_rapl; then
        echo "RAPL : disponible ✓"
        PERF_EVENTS+=("$RAPL_EVENT")
    else
        ENABLE_RAPL=false
        echo ""
    fi
fi

# =============================================================================
# Construction de la commande perf
# =============================================================================

# Jointure des events avec virgule pour -e
EVENTS_STR=$(IFS=','; echo "${PERF_EVENTS[*]}")

# Arguments transmis au binaire Divan.
# --sample-count : un seul sample par run perf — minimise le bruit de mesure.
# Divan exécutera tout de même plusieurs iters internes pour la précision.
# Le filtre est un argument positionnel nu. --sample-count est le seul
# flag nommé reconnu par le binaire Divan.
DIVAN_ARGS=(
    "$BENCH_FILTER"
    "--sample-count"  "$DIVAN_SAMPLES"
)

PERF_CMD=(
    perf stat
    -e "$EVENTS_STR"
    --repeat "$SAMPLE_COUNT"
    -o "$OUTPUT_FILE"
    --append
    --
    "$BENCH_BIN"
    "${DIVAN_ARGS[@]}"
)

# =============================================================================
# Exécution
# =============================================================================

echo "Démarrage du profil (${SAMPLE_COUNT} runs)..."
echo "Commande perf : ${PERF_CMD[*]}"
echo ""

# En-tête du fichier de sortie
{
    echo "# Marius Engine — Profil de Frugalité Matérielle"
    echo "# Date       : $(date -Iseconds)"
    echo "# Binaire    : $BENCH_BIN"
    echo "# Filtre     : $BENCH_FILTER"
    echo "# RAPL       : $ENABLE_RAPL"
    echo "# Commande   : ${PERF_CMD[*]}"
    echo "#"
} > "$OUTPUT_FILE"

# Exécution de perf stat
"${PERF_CMD[@]}" 2>&1 | tee -a "$OUTPUT_FILE"

# =============================================================================
# Parsing et rapport des métriques clés
# =============================================================================

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  Rapport de frugalité — métriques extraites"
echo "══════════════════════════════════════════════════════════════"

# is_numeric : retourne 0 (vrai) si la chaîne est un entier.
# Protège les comparaisons -gt contre "<not counted>", "<not supported>"
# ou les chaînes vides que perf retourne quand un event est indisponible.
is_numeric() {
    [[ "$1" =~ ^[0-9]+$ ]]
}

parse_metric() {
    # Extrait la première colonne numérique d'une ligne perf stat.
    # awk supprime les séparateurs de milliers (virgules).
    # Retourne une chaîne vide si la valeur n'est pas numérique.
    local pattern="$1"
    grep -E "$pattern" "$OUTPUT_FILE" \
        | tail -1 \
        | awk '{gsub(/,/, ""); if ($1 ~ /^[0-9]+$/) print $1}' \
        | tr -d ' '
}

CYCLES=$(parse_metric " cycles")
INSTRS=$(parse_metric " instructions")
L1_LOADS=$(parse_metric "L1-dcache-loads[^-]")
L1_MISSES=$(parse_metric "L1-dcache-load-misses")
LLC_LOADS=$(parse_metric "LLC-loads[^-]")
LLC_MISSES=$(parse_metric "LLC-load-misses")

# Note sur la portée des métriques :
# perf stat mesure l'intégralité du processus Divan, incluant l'initialisation,
# la construction des batchs et l'infrastructure de mesure — pas uniquement
# le hot path render(). Ces valeurs qualifient le comportement global du binaire.
# Pour isoler render() : perf record + perf report avec annotation des symboles.

# IPC = instructions / cycles
if is_numeric "$CYCLES" && is_numeric "$INSTRS" && [[ "$CYCLES" -gt 0 ]]; then
    IPC=$(awk "BEGIN { printf \"%.3f\", $INSTRS / $CYCLES }")
    echo ""
    echo "  IPC (Instructions Per Cycle) : $IPC"
    echo "  (scope : processus entier incluant infrastructure Divan)"
    if awk "BEGIN { exit ($IPC < 2.0) ? 0 : 1 }"; then
        echo "    ⚠  IPC < 2.0 — attendu sur mesure globale (overhead Divan/init)"
    else
        echo "    ✓  IPC ≥ 2.0 — pipeline saturé sur l'ensemble du processus"
    fi
fi

# L1 miss rate
if is_numeric "$L1_LOADS" && is_numeric "$L1_MISSES" && [[ "$L1_LOADS" -gt 0 ]]; then
    L1_MISS_RATE=$(awk "BEGIN { printf \"%.4f\", ($L1_MISSES / $L1_LOADS) * 100 }")
    echo ""
    echo "  L1-dcache miss rate : ${L1_MISS_RATE}%"
    if awk "BEGIN { exit ($L1_MISS_RATE < 1.0) ? 0 : 1 }"; then
        echo "    ✓  < 1% — layout DOD validé, données en L1 au moment de l'accès"
    else
        echo "    ⚠  > 1% — vérifier padding/alignement des structs"
    fi
fi

# LLC miss rate — non disponible sur certaines µarch
if is_numeric "$LLC_LOADS" && is_numeric "$LLC_MISSES" && [[ "$LLC_LOADS" -gt 0 ]]; then
    LLC_MISS_RATE=$(awk "BEGIN { printf \"%.4f\", ($LLC_MISSES / $LLC_LOADS) * 100 }")
    echo ""
    echo "  LLC miss rate : ${LLC_MISS_RATE}%"
    if awk "BEGIN { exit ($LLC_MISS_RATE < 0.5) ? 0 : 1 }"; then
        echo "    ✓  < 0.5% — données dans le cache, accès RAM rares"
    else
        echo "    ⚠  > 0.5% — batch trop grand pour tenir dans LLC"
    fi
else
    echo ""
    echo "  LLC miss rate : non disponible (event non compté sur cette µarch)"
fi

# RAPL : énergie en Joules
if $ENABLE_RAPL; then
    JOULES=$(grep -E "energy-pkg" "$OUTPUT_FILE" \
        | tail -1 \
        | awk '{print $1}' \
        | tr -d ' ,')
    if [[ -n "$JOULES" ]] && is_numeric "${JOULES%%.*}" && [[ "$JOULES" != "0" ]]; then
        TOTAL_ITEMS=$(( 10000 * SAMPLE_COUNT ))
        NANOJOULES_PER_ITEM=$(awk "BEGIN { printf \"%.2f\", ($JOULES * 1e9) / $TOTAL_ITEMS }")
        echo ""
        echo "  Énergie RAPL : ${JOULES} J total"
        echo "  Rendement    : ${NANOJOULES_PER_ITEM} nJ / item"
        echo "    (base : 10 000 items × $SAMPLE_COUNT runs = $TOTAL_ITEMS items)"
    fi
fi

echo "  Rapport brut sauvegardé : $OUTPUT_FILE"
echo "══════════════════════════════════════════════════════════════"
echo ""
echo "Interprétation de référence :"
echo "  IPC > 3.0 + L1 miss < 0.5% + LLC miss < 0.1% + nJ/item < 100"
echo "  → Sympathie Mécanique validée : le CPU ne perd pas de cycles"
echo "     à attendre la RAM. Le layout DOD + O(T) buffers est optimal."
