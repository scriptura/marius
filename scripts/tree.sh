#!/bin/bash
export LC_ALL=C.UTF-8
export LANG=C.UTF-8
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
PROJECT_NAME="$(basename "$PROJECT_ROOT")"
DOCS_DIR="$PROJECT_ROOT/docs"
GEN_DIR="$DOCS_DIR/generate"
OUTPUT="$GEN_DIR/tree.md"
TIMESTAMP=$(date +"%Y-%m-%d %H:%M:%S")

mkdir -p "$GEN_DIR"

cat > "$OUTPUT" << EOF
# Structure du Projet

**Généré le:** $TIMESTAMP

## Racine du Workspace

\`\`\`text
EOF

declare -A PRUNE=(
    [crates]="target build artifacts"
    [scripts]=""
    [db]="doc"
)

(
    cd "$PROJECT_ROOT"
    {
        find . -maxdepth 1 -type f -print
        for d in "${!PRUNE[@]}"; do
            [ -d "$d" ] || continue
            if [ -n "${PRUNE[$d]}" ]; then
                args=()
                for n in ${PRUNE[$d]}; do args+=( -name "$n" -o ); done
                unset 'args[${#args[@]}-1]'
                find "$d" \( "${args[@]}" \) -prune -o -print 2>/dev/null
            else
                find "$d" -print 2>/dev/null
            fi
        done
    } | tree --fromfile --dirsfirst
) | sed "1s|.*|/$PROJECT_NAME|" >> "$OUTPUT"

cat >> "$OUTPUT" << EOF
\`\`\`

## Documentation du projet

\`\`\`text
EOF

if [ -d "$DOCS_DIR" ]; then
    tree "$DOCS_DIR" --dirsfirst | sed "1s|.*|/$PROJECT_NAME/docs|" >> "$OUTPUT"
else
    echo "⚠️ Dossier $DOCS_DIR introuvable." >> "$OUTPUT"
fi

cat >> "$OUTPUT" << EOF
\`\`\`

EOF

echo -e "\033[0;32m✓\033[0m Arborescence générée : docs/generate/tree.md"
