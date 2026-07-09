#!/bin/bash
export LC_ALL=C.UTF-8
export LANG=C.UTF-8
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOCS_DIR="$PROJECT_ROOT/docs"
GEN_DIR="$DOCS_DIR/generate"
OUTPUT="$GEN_DIR/tree.md"
TIMESTAMP=$(date +"%Y-%m-%d %H:%M:%S")

mkdir -p "$GEN_DIR"

cat > "$OUTPUT" << EOF
# Structure du Projet

**Généré le:** $TIMESTAMP

---

## 📁 Racine du Workspace

\`\`\`text
EOF

tree "$PROJECT_ROOT" -I "target|README.md|doc|docs|logs|artifacts|archives" --dirsfirst >> "$OUTPUT"

cat >> "$OUTPUT" << 'EOF'

```

---

## 📁 Documentation

```text
EOF

if [ -d "$DOCS_DIR" ]; then
    tree "$DOCS_DIR" -I "implementation-history" --dirsfirst >> "$OUTPUT"
else
    echo "⚠️ Dossier $DOCS_DIR introuvable." >> "$OUTPUT"
fi

cat >> "$OUTPUT" << 'EOF'

```

EOF

echo -e "\033[0;32m✓\033[0m Arborescence générée : docs/generate/tree.md"
